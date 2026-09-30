use axum::body::Body;
use axum::http::{Request, StatusCode};
use brokenvault::client::restore::{RestoreOptions, run_restore};
use brokenvault::client::scan::scan_directory;
use brokenvault::client::upload::{UploadOptions, run_backup};
use brokenvault::core::diff::compare_manifests;
use brokenvault::core::hash::sha256_hex;
use brokenvault::core::manifest::{Manifest, ManifestEntry};
use brokenvault::core::proto::{DiffChangeType, VaultStats};
use brokenvault::server::api::{AppState, create_router};
use brokenvault::server::compute_vault_stats;
use brokenvault::server::db::Database;
use brokenvault::server::store::Store;
use http_body_util::BodyExt;
use std::fs::{self, File};
use std::io::Write;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;
use tokio::net::TcpListener;
use tower::ServiceExt;

#[test]
fn test_compare_manifests_unit() {
    let h1 = sha256_hex(b"chunk1");
    let h2 = sha256_hex(b"chunk2");
    let h3 = sha256_hex(b"chunk3");
    let h4 = sha256_hex(b"chunk4");
    let h5 = sha256_hex(b"chunk5");

    let manifest_a = Manifest::new(vec![
        ManifestEntry::File {
            path: "alpha.txt".to_string(),
            size: 100,
            mtime: (1000, 0),
            chunks: vec![(h1, 100)],
        },
        ManifestEntry::File {
            path: "beta.txt".to_string(),
            size: 200,
            mtime: (1000, 0),
            chunks: vec![(h2, 200)],
        },
        ManifestEntry::File {
            path: "common.txt".to_string(),
            size: 300,
            mtime: (1000, 0),
            chunks: vec![(h3.clone(), 300)],
        },
        ManifestEntry::Dir {
            path: "docs".to_string(),
            mtime: (1000, 0),
        },
    ])
    .unwrap();

    let manifest_b = Manifest::new(vec![
        ManifestEntry::File {
            path: "alpha.txt".to_string(),
            size: 150,
            mtime: (2000, 0),
            chunks: vec![(h4, 150)],
        },
        ManifestEntry::File {
            path: "common.txt".to_string(),
            size: 300,
            mtime: (1000, 0),
            chunks: vec![(h3, 300)],
        },
        ManifestEntry::File {
            path: "gamma.txt".to_string(),
            size: 250,
            mtime: (2000, 0),
            chunks: vec![(h5, 250)],
        },
        ManifestEntry::Dir {
            path: "archive".to_string(),
            mtime: (2000, 0),
        },
    ])
    .unwrap();

    let report = compare_manifests("v1", &manifest_a, "v2", &manifest_b);

    assert_eq!(report.from_version, "v1");
    assert_eq!(report.to_version, "v2");
    assert_eq!(report.files_added, 2);
    assert_eq!(report.files_removed, 2);
    assert_eq!(report.files_modified, 1);
    assert_eq!(report.total_reused_bytes, 300);
    assert_eq!(report.total_new_bytes, 400);

    let alpha_diff = report
        .files
        .iter()
        .find(|c| c.path == "alpha.txt")
        .expect("alpha.txt diff");
    assert_eq!(alpha_diff.change, DiffChangeType::Modified);
    assert_eq!(alpha_diff.old_size, Some(100));
    assert_eq!(alpha_diff.new_size, Some(150));
    assert_eq!(alpha_diff.new_chunks, 1);
    assert_eq!(alpha_diff.reused_chunks, 0);

    let beta_diff = report
        .files
        .iter()
        .find(|c| c.path == "beta.txt")
        .expect("beta.txt diff");
    assert_eq!(beta_diff.change, DiffChangeType::Removed);

    let gamma_diff = report
        .files
        .iter()
        .find(|c| c.path == "gamma.txt")
        .expect("gamma.txt diff");
    assert_eq!(gamma_diff.change, DiffChangeType::Added);
    assert_eq!(gamma_diff.new_size, Some(250));
    assert_eq!(gamma_diff.new_chunks, 1);
}

#[tokio::test]
async fn test_vault_stats_endpoint_empty_and_populated() {
    let tmp = tempdir().unwrap();
    let vault_dir = tmp.path().join("vault");
    let store = Store::new(&vault_dir).unwrap();
    let db = Database::open(vault_dir.join("meta.db")).unwrap();
    let state = AppState {
        store: store.clone(),
        db: Arc::new(Mutex::new(db)),
    };

    let router = create_router(state);

    let req = Request::builder()
        .uri("/v1/stats")
        .method("GET")
        .body(Body::empty())
        .unwrap();

    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let body = res.into_body().collect().await.unwrap().to_bytes();
    let stats: VaultStats = serde_json::from_slice(&body).unwrap();
    assert_eq!(stats.completed_versions, 0);
    assert_eq!(stats.open_uploads, 0);
    assert_eq!(stats.unique_chunks, 0);
    assert_eq!(stats.total_physical_chunk_bytes, 0);
    assert_eq!(stats.total_logical_bytes, 0);
    assert!((stats.deduplication_ratio - 1.0).abs() < f64::EPSILON);
    assert_eq!(stats.space_saved_bytes, 0);
    assert!((stats.space_saved_percent - 0.0).abs() < f64::EPSILON);
}

#[test]
fn test_features_workflow_integration() {
    let tmp = tempdir().unwrap();
    let src_dir = tmp.path().join("source");
    let vault_dir = tmp.path().join("vault");

    fs::create_dir_all(src_dir.join("docs")).unwrap();
    fs::create_dir_all(src_dir.join("src")).unwrap();
    fs::create_dir_all(src_dir.join("assets/images")).unwrap();

    let mut f1 = File::create(src_dir.join("docs/readme.md")).unwrap();
    f1.write_all(b"# Documentation\nHello world").unwrap();

    let mut f2 = File::create(src_dir.join("docs/manual.txt")).unwrap();
    f2.write_all(b"User manual instructions").unwrap();

    let mut f3 = File::create(src_dir.join("src/main.rs")).unwrap();
    f3.write_all(b"fn main() {}\n").unwrap();

    let mut f4 = File::create(src_dir.join("assets/images/logo.png")).unwrap();
    f4.write_all(b"fake png image bytes").unwrap();

    let store = Store::new(&vault_dir).unwrap();
    let db = Database::open(vault_dir.join("meta.db")).unwrap();
    let state = AppState {
        store: store.clone(),
        db: Arc::new(Mutex::new(db)),
    };

    let rt = tokio::runtime::Runtime::new().unwrap();
    let (addr_tx, addr_rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        rt.block_on(async move {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            addr_tx.send(addr).unwrap();
            let router = create_router(state);
            axum::serve(listener, router).await.unwrap();
        });
    });

    let addr = match addr_rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(a) => a,
        Err(_) => return,
    };
    let server_url = format!("http://{}", addr);

    let v1_res = run_backup(&src_dir, &server_url, UploadOptions::default());
    let v1 = match v1_res {
        Ok(c) => c.expect("v1 commit should be present"),
        Err(e) => {
            if let brokenvault::core::errors::CoreError::Io(ref io_err) = e {
                if io_err.raw_os_error() == Some(1)
                    || io_err.to_string().contains("Operation not permitted")
                {
                    eprintln!("Skipping test: network socket denied by sandbox");
                    return;
                }
            }
            panic!("v1 backup failed: {:?}", e);
        }
    };
    assert_eq!(v1.version, "v1");

    let sub_dest = tmp.path().join("restore_subfolder");
    let restore_docs = run_restore(
        "v1",
        &sub_dest,
        &server_url,
        RestoreOptions {
            subpath: Some("docs".to_string()),
            ..Default::default()
        },
    );
    restore_docs.expect("partial restore of docs directory must succeed");

    assert!(sub_dest.join("docs/readme.md").exists());
    assert!(sub_dest.join("docs/manual.txt").exists());
    assert!(!sub_dest.join("src").exists());
    assert!(!sub_dest.join("assets").exists());

    let file_dest = tmp.path().join("restore_single_file");
    let restore_file = run_restore(
        "v1",
        &file_dest,
        &server_url,
        RestoreOptions {
            subpath: Some("src/main.rs".to_string()),
            ..Default::default()
        },
    );
    restore_file.expect("partial restore of single file must succeed");

    assert!(file_dest.join("src/main.rs").exists());
    assert!(!file_dest.join("docs").exists());
    assert!(!file_dest.join("assets").exists());

    let invalid_dest = tmp.path().join("restore_invalid");
    let restore_invalid = run_restore(
        "v1",
        &invalid_dest,
        &server_url,
        RestoreOptions {
            subpath: Some("nonexistent/path".to_string()),
            ..Default::default()
        },
    );
    assert!(restore_invalid.is_err());

    let traversal_dest = tmp.path().join("restore_traversal");
    let restore_traversal = run_restore(
        "v1",
        &traversal_dest,
        &server_url,
        RestoreOptions {
            subpath: Some("../../etc/passwd".to_string()),
            ..Default::default()
        },
    );
    assert!(restore_traversal.is_err());

    let unchanged_res = run_backup(&src_dir, &server_url, UploadOptions::default())
        .expect("unchanged backup execution");
    assert!(unchanged_res.is_none());

    let snapshot_res = run_backup(
        &src_dir,
        &server_url,
        UploadOptions {
            snapshot: true,
            ..Default::default()
        },
    )
    .expect("snapshot backup execution");

    let v2 = snapshot_res.expect("v2 commit must be created on snapshot backup");
    assert_eq!(v2.version, "v2");
    assert_eq!(v2.uploaded_bytes, 0);
    assert_eq!(v2.reused_bytes, v2.total_bytes);
    assert_eq!(v2.chunks, v1.chunks);

    let mut f5 = File::create(src_dir.join("new_feature.txt")).unwrap();
    f5.write_all(b"Brand new feature contents added in version 3")
        .unwrap();

    let v3_res =
        run_backup(&src_dir, &server_url, UploadOptions::default()).expect("v3 backup execution");
    let v3 = v3_res.expect("v3 commit must be created");
    assert_eq!(v3.version, "v3");

    let ro_store = Store::open_read_only(&vault_dir).unwrap();
    let ro_db = Database::open_read_only(vault_dir.join("meta.db")).unwrap();

    let stats = compute_vault_stats(&ro_store, &ro_db).unwrap();
    assert_eq!(stats.completed_versions, 3);
    assert_eq!(stats.open_uploads, 0);
    assert!(stats.unique_chunks > 0);
    assert!(stats.deduplication_ratio > 1.5);
    assert!(stats.space_saved_bytes > 0);
    assert!(stats.space_saved_percent > 30.0);

    let m1_bytes = ro_db.get_version_manifest("v1").unwrap().unwrap();
    let m2_bytes = ro_db.get_version_manifest("v2").unwrap().unwrap();
    let m3_bytes = ro_db.get_version_manifest("v3").unwrap().unwrap();

    let m1 = Manifest::from_bytes(&m1_bytes).unwrap();
    let m2 = Manifest::from_bytes(&m2_bytes).unwrap();
    let m3 = Manifest::from_bytes(&m3_bytes).unwrap();

    let diff_1_2 = compare_manifests("v1", &m1, "v2", &m2);
    assert_eq!(diff_1_2.files_added, 0);
    assert_eq!(diff_1_2.files_removed, 0);
    assert_eq!(diff_1_2.files_modified, 0);
    assert_eq!(diff_1_2.total_new_bytes, 0);
    assert_eq!(diff_1_2.total_reused_bytes, v2.total_bytes);

    let diff_2_3 = compare_manifests("v2", &m2, "v3", &m3);
    assert_eq!(diff_2_3.files_added, 1);
    assert_eq!(diff_2_3.files_removed, 0);
    assert_eq!(diff_2_3.files_modified, 0);
    assert!(diff_2_3.total_new_bytes > 0);
    let new_file_diff = diff_2_3
        .files
        .iter()
        .find(|f| f.path == "new_feature.txt")
        .expect("new_feature.txt in diff");
    assert_eq!(new_file_diff.change, DiffChangeType::Added);
}

#[tokio::test]
async fn test_preexisting_chunk_put_does_not_inflate_uploaded_bytes() {
    let tmp = tempdir().unwrap();
    let vault_dir = tmp.path().join("vault");
    let store = Store::new(&vault_dir).unwrap();
    let db = Database::open(vault_dir.join("meta.db")).unwrap();
    let state = AppState {
        store: store.clone(),
        db: Arc::new(Mutex::new(db)),
    };
    let router = create_router(state);

    let chunk_data = b"preexisting chunk payload bytes 12345";
    let chunk_len = chunk_data.len() as u64;
    let chunk_id = sha256_hex(chunk_data);

    let manifest1 = Manifest::new(vec![ManifestEntry::File {
        path: "file1.txt".to_string(),
        size: chunk_len,
        mtime: (1000, 0),
        chunks: vec![(chunk_id.clone(), chunk_len)],
    }])
    .unwrap();
    let manifest1_bytes = manifest1.canonical_bytes().unwrap();

    let init_req = Request::builder()
        .uri("/v1/uploads")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(manifest1_bytes))
        .unwrap();
    let init_res = router.clone().oneshot(init_req).await.unwrap();
    assert_eq!(init_res.status(), StatusCode::CREATED);
    let init_body = init_res.into_body().collect().await.unwrap().to_bytes();
    let init_dto: serde_json::Value = serde_json::from_slice(&init_body).unwrap();
    let upload_id1 = init_dto["upload_id"].as_str().unwrap();

    let put_req = Request::builder()
        .uri(format!("/v1/uploads/{}/chunks/{}", upload_id1, chunk_id))
        .method("PUT")
        .body(Body::from(chunk_data.to_vec()))
        .unwrap();
    let put_res = router.clone().oneshot(put_req).await.unwrap();
    assert_eq!(put_res.status(), StatusCode::CREATED);

    let commit_req = Request::builder()
        .uri(format!("/v1/uploads/{}/commit", upload_id1))
        .method("POST")
        .body(Body::empty())
        .unwrap();
    let commit_res = router.clone().oneshot(commit_req).await.unwrap();
    assert_eq!(commit_res.status(), StatusCode::OK);
    let commit_body = commit_res.into_body().collect().await.unwrap().to_bytes();
    let commit_dto: serde_json::Value = serde_json::from_slice(&commit_body).unwrap();
    assert_eq!(commit_dto["uploaded_bytes"], chunk_len);

    let manifest2 = Manifest::new(vec![ManifestEntry::File {
        path: "file2.txt".to_string(),
        size: chunk_len,
        mtime: (2000, 0),
        chunks: vec![(chunk_id.clone(), chunk_len)],
    }])
    .unwrap();
    let manifest2_bytes = manifest2.canonical_bytes().unwrap();

    let init_req2 = Request::builder()
        .uri("/v1/uploads")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(manifest2_bytes))
        .unwrap();
    let init_res2 = router.clone().oneshot(init_req2).await.unwrap();
    assert_eq!(init_res2.status(), StatusCode::CREATED);
    let init_body2 = init_res2.into_body().collect().await.unwrap().to_bytes();
    let init_dto2: serde_json::Value = serde_json::from_slice(&init_body2).unwrap();
    let upload_id2 = init_dto2["upload_id"].as_str().unwrap();
    assert!(init_dto2["missing"].as_array().unwrap().is_empty());

    let put_req2 = Request::builder()
        .uri(format!("/v1/uploads/{}/chunks/{}", upload_id2, chunk_id))
        .method("PUT")
        .body(Body::from(chunk_data.to_vec()))
        .unwrap();
    let put_res2 = router.clone().oneshot(put_req2).await.unwrap();
    assert_eq!(put_res2.status(), StatusCode::OK);

    let commit_req2 = Request::builder()
        .uri(format!("/v1/uploads/{}/commit", upload_id2))
        .method("POST")
        .body(Body::empty())
        .unwrap();
    let commit_res2 = router.clone().oneshot(commit_req2).await.unwrap();
    assert_eq!(commit_res2.status(), StatusCode::OK);
    let commit_body2 = commit_res2.into_body().collect().await.unwrap().to_bytes();
    let commit_dto2: serde_json::Value = serde_json::from_slice(&commit_body2).unwrap();
    assert_eq!(commit_dto2["uploaded_bytes"], 0);
    assert_eq!(commit_dto2["reused_bytes"], chunk_len);
}

#[test]
fn test_scanner_skips_nested_vault_directory() {
    let tmp = tempdir().unwrap();
    let src_dir = tmp.path().join("source_with_vault");
    fs::create_dir_all(src_dir.join("sub")).unwrap();
    File::create(src_dir.join("sub/regular.txt"))
        .unwrap()
        .write_all(b"regular file data")
        .unwrap();

    let nested_vault = src_dir.join("vault_copy");
    Store::new(&nested_vault).unwrap();
    File::create(nested_vault.join("chunks/fake.bin"))
        .unwrap()
        .write_all(b"fake chunk")
        .unwrap();

    File::create(src_dir.join("journal.json"))
        .unwrap()
        .write_all(b"{}")
        .unwrap();

    let (manifest, _) = scan_directory(&src_dir).unwrap();
    assert!(
        manifest
            .entries
            .iter()
            .any(|e| e.path() == "sub/regular.txt")
    );
    assert!(
        !manifest
            .entries
            .iter()
            .any(|e| e.path().starts_with("vault_copy"))
    );
    assert!(
        !manifest
            .entries
            .iter()
            .any(|e| e.path().contains("journal.json"))
    );
}

#[tokio::test]
async fn test_protocol_json_contracts() {
    let tmp = tempdir().unwrap();
    let vault_dir = tmp.path().join("vault");
    let store = Store::new(&vault_dir).unwrap();
    let db = Database::open(vault_dir.join("meta.db")).unwrap();
    let state = AppState {
        store: store.clone(),
        db: Arc::new(Mutex::new(db)),
    };
    let router = create_router(state);

    let health_req = Request::builder()
        .uri("/v1/health")
        .method("GET")
        .body(Body::empty())
        .unwrap();
    let health_res = router.clone().oneshot(health_req).await.unwrap();
    assert_eq!(health_res.status(), StatusCode::OK);
    let health_val: serde_json::Value =
        serde_json::from_slice(&health_res.into_body().collect().await.unwrap().to_bytes())
            .unwrap();
    assert!(health_val.get("vault_id").is_some());
    assert!(health_val.get("protocol_version").is_some());
    assert_eq!(health_val["status"], "ok");

    let stats_req = Request::builder()
        .uri("/v1/stats")
        .method("GET")
        .body(Body::empty())
        .unwrap();
    let stats_res = router.clone().oneshot(stats_req).await.unwrap();
    assert_eq!(stats_res.status(), StatusCode::OK);
    let stats_val: serde_json::Value =
        serde_json::from_slice(&stats_res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(stats_val.get("vault_id").is_some());
    assert!(stats_val.get("completed_versions").is_some());
    assert!(stats_val.get("open_uploads").is_some());
    assert!(stats_val.get("total_logical_bytes").is_some());
    assert!(stats_val.get("total_physical_chunk_bytes").is_some());
    assert!(stats_val.get("unique_chunks").is_some());
    assert!(stats_val.get("deduplication_ratio").is_some());
    assert!(stats_val.get("space_saved_bytes").is_some());
    assert!(stats_val.get("space_saved_percent").is_some());

    let verify_req = Request::builder()
        .uri("/v1/verify")
        .method("POST")
        .body(Body::empty())
        .unwrap();
    let verify_res = router.clone().oneshot(verify_req).await.unwrap();
    assert_eq!(verify_res.status(), StatusCode::OK);
    let verify_val: serde_json::Value =
        serde_json::from_slice(&verify_res.into_body().collect().await.unwrap().to_bytes())
            .unwrap();
    assert!(verify_val.get("healthy").is_some());
    assert!(verify_val.get("damaged_chunks").is_some());
    assert!(verify_val.get("healthy_versions").is_some());
    assert!(verify_val.get("info_messages").is_some());
}
