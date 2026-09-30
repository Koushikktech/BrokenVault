use brokenvault::client::devtools::{diff_directories, generate_sample_dataset};
use brokenvault::client::restore::{RestoreOptions, run_restore};
use brokenvault::client::upload::{UploadOptions, run_backup};
use brokenvault::core::proto::{OpenUploadSummary, UploadStatusResponse, VersionSummary};
use brokenvault::server::api::{AppState, create_router};
use brokenvault::server::db::Database;
use brokenvault::server::store::Store;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

#[test]
fn test_seeded_crash_chaos_recovery() {
    let tmp = tempdir().unwrap();
    let src_dir = tmp.path().join("source");
    let vault_dir = tmp.path().join("vault");
    let restore_dir = tmp.path().join("restored");
    let state_dir = tmp.path().join("state");

    unsafe {
        std::env::set_var("BV_STATE_DIR", &state_dir);
    }

    generate_sample_dataset(&src_dir, 1001, false).unwrap();

    let store1 = Store::new(&vault_dir).unwrap();
    let db1 = Database::open(vault_dir.join("meta.db")).unwrap();
    let state1 = AppState {
        store: store1,
        db: Arc::new(Mutex::new(db1)),
    };

    let (shutdown_tx1, shutdown_rx1) = oneshot::channel::<()>();
    let (addr_tx1, addr_rx1) = std::sync::mpsc::channel();

    let server_thread1 = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let listener = match TcpListener::bind("127.0.0.1:0").await {
                Ok(listener) => listener,
                Err(e) => {
                    addr_tx1.send(Err(e)).unwrap();
                    return;
                }
            };
            addr_tx1.send(Ok(listener.local_addr().unwrap())).unwrap();
            let router = create_router(state1);
            axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx1.await;
                })
                .await
                .unwrap();
        });
    });

    let addr1 = match addr_rx1
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("server bind result")
    {
        Ok(addr) => addr,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("SKIP crash chaos: sandbox denies loopback bind: {e}");
            server_thread1.join().unwrap();
            return;
        }
        Err(e) => panic!("server bind failed: {e}"),
    };
    let server_url1 = format!("http://{}", addr1);

    let partial_res = run_backup(
        &src_dir,
        &server_url1,
        UploadOptions {
            jobs: 1,
            stop_after_chunks: Some(3),
        },
    );

    assert!(
        matches!(
            partial_res,
            Err(brokenvault::core::errors::CoreError::Interrupted)
        ),
        "expected controlled interruption, got {:?}",
        partial_res
    );

    let client = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .new_agent();

    let mut versions_check = client
        .get(&format!("{}/v1/versions", server_url1))
        .call()
        .unwrap();
    let versions_list: Vec<VersionSummary> = versions_check.body_mut().read_json().unwrap();
    assert!(
        versions_list.is_empty(),
        "unfinished upload must not appear in versions list"
    );

    let mut uploads_check = client
        .get(&format!("{}/v1/uploads", server_url1))
        .call()
        .unwrap();
    let open_uploads: Vec<OpenUploadSummary> = uploads_check.body_mut().read_json().unwrap();
    assert_eq!(
        open_uploads.len(),
        1,
        "open upload session must be tracked by server"
    );
    assert_eq!(open_uploads[0].state, "open");
    assert!(open_uploads[0].chunks_present > 0);
    assert!(open_uploads[0].chunks_present < open_uploads[0].chunks_total);
    let upload_id = open_uploads[0].upload_id.clone();
    let mut status_before = client
        .get(&format!("{server_url1}/v1/uploads/{upload_id}"))
        .call()
        .unwrap();
    let before: UploadStatusResponse = status_before.body_mut().read_json().unwrap();
    assert_eq!(before.state, "open");
    let missing: HashSet<_> = before.missing.into_iter().collect();
    assert!(!missing.is_empty());

    let _ = shutdown_tx1.send(());
    let _ = server_thread1.join();

    let store2 = Store::new(&vault_dir).unwrap();
    let db2 = Database::open(vault_dir.join("meta.db")).unwrap();
    let state2 = AppState {
        store: store2,
        db: Arc::new(Mutex::new(db2)),
    };

    let (shutdown_tx2, shutdown_rx2) = oneshot::channel::<()>();
    let (addr_tx2, addr_rx2) = std::sync::mpsc::channel();

    let server_thread2 = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("restart bind");
            addr_tx2.send(listener.local_addr().unwrap()).unwrap();
            let router = create_router(state2);
            axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx2.await;
                })
                .await
                .unwrap();
        });
    });

    let addr2 = match addr_rx2.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(a) => a,
        Err(e) => panic!("restart server did not bind: {e}"),
    };
    let server_url2 = format!("http://{}", addr2);

    let mut uploads_after_restart = client
        .get(&format!("{server_url2}/v1/uploads"))
        .call()
        .unwrap();
    let open_after: Vec<OpenUploadSummary> = uploads_after_restart.body_mut().read_json().unwrap();
    assert_eq!(open_after.len(), 1);
    assert_eq!(open_after[0].upload_id, upload_id);
    let mut status_after = client
        .get(&format!("{server_url2}/v1/uploads/{upload_id}"))
        .call()
        .unwrap();
    let after: UploadStatusResponse = status_after.body_mut().read_json().unwrap();
    assert_eq!(after.state, "open");
    assert_eq!(after.missing.into_iter().collect::<HashSet<_>>(), missing);
    let mut versions_before_resume = client
        .get(&format!("{server_url2}/v1/versions"))
        .call()
        .unwrap();
    assert!(
        versions_before_resume
            .body_mut()
            .read_json::<Vec<VersionSummary>>()
            .unwrap()
            .is_empty()
    );

    let resumed_res = run_backup(&src_dir, &server_url2, UploadOptions::default());
    let commit = resumed_res
        .expect("resumed backup must succeed")
        .expect("v1 commit must be returned");
    assert_eq!(commit.version, "v1");

    let mut versions_after = client
        .get(&format!("{}/v1/versions", server_url2))
        .call()
        .unwrap();
    let versions_final: Vec<VersionSummary> = versions_after.body_mut().read_json().unwrap();
    assert_eq!(versions_final.len(), 1);
    assert_eq!(versions_final[0].id, "v1");
    assert_eq!(versions_final[0].upload_id, upload_id);
    assert!(versions_final[0].uploaded_bytes > 0);
    let mut no_open = client
        .get(&format!("{server_url2}/v1/uploads"))
        .call()
        .unwrap();
    assert!(
        no_open
            .body_mut()
            .read_json::<Vec<OpenUploadSummary>>()
            .unwrap()
            .is_empty()
    );

    let restore_res = run_restore("v1", &restore_dir, &server_url2, RestoreOptions::default());
    restore_res.expect("restore must succeed");

    let identical = diff_directories(&src_dir, &restore_dir).unwrap();
    assert!(
        identical,
        "restored directory must match source exactly after chaos"
    );

    let _ = shutdown_tx2.send(());
    let _ = server_thread2.join();
}
