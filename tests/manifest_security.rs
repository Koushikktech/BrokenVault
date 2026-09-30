use axum::body::Body;
use axum::http::{Request, StatusCode};
use brokenvault::core::errors::ManifestError;
use brokenvault::core::hash::sha256_hex;
use brokenvault::core::manifest::{ChunkerConfig, Manifest, ManifestEntry};
use brokenvault::core::proto::UploadInitResponse;
use brokenvault::server::api::{AppState, create_router};
use brokenvault::server::db::Database;
use brokenvault::server::store::Store;
use http_body_util::BodyExt;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;
use tower::ServiceExt;

fn file(path: &str, id: &str, len: u64) -> ManifestEntry {
    ManifestEntry::File {
        path: path.to_string(),
        size: len,
        mtime: (1, 0),
        chunks: vec![(id.to_string(), len)],
    }
}

#[test]
fn rejects_invalid_ids_inconsistent_lengths_and_unsupported_chunkers() {
    let id = "a".repeat(64);
    for bad in [
        "hash1".to_string(),
        "A".repeat(64),
        "g".repeat(64),
        "é".repeat(32),
        "../escape".to_string(),
    ] {
        assert!(
            Manifest::new(vec![file("a", &bad, 1)]).is_err(),
            "accepted {bad}"
        );
    }
    let mut manifest = Manifest::new(vec![file("a", &id, 1), file("b", &id, 1)]).unwrap();
    if let ManifestEntry::File { size, chunks, .. } = &mut manifest.entries[1] {
        *size = 2;
        chunks[0].1 = 2;
    }
    assert!(manifest.validate().is_err());

    let mut config = ChunkerConfig::default();
    config.avg += 1;
    manifest.entries = vec![file("a", &id, 1)];
    manifest.chunker = config;
    assert!(matches!(
        manifest.validate(),
        Err(ManifestError::InvalidChunker(_))
    ));

    let mut mismatch = file("a", "hash1", 1);
    if let ManifestEntry::File { size, .. } = &mut mismatch {
        *size = 2;
    }
    assert!(matches!(
        Manifest::new(vec![mismatch]),
        Err(ManifestError::ChunkSumMismatch { .. })
    ));
}

#[test]
fn checked_sums_reject_overflow() {
    let id = "a".repeat(64);
    let other = "b".repeat(64);
    let entry = ManifestEntry::File {
        path: "a".to_string(),
        size: 0,
        mtime: (0, 0),
        chunks: vec![(id.clone(), u64::MAX), (other, 1)],
    };
    assert!(matches!(
        Manifest::new(vec![entry]),
        Err(ManifestError::InvalidFormat(_))
    ));
    assert!(matches!(
        Manifest::new(vec![file("a", &id, u64::MAX), file("b", &id, u64::MAX)]),
        Err(ManifestError::InvalidFormat(_))
    ));
}

#[test]
fn public_store_methods_reject_unsafe_ids_and_outside_tmp_paths() {
    let tmp = tempdir().unwrap();
    let store = Store::new(tmp.path().join("vault")).unwrap();
    let outside = tmp.path().join("outside");
    std::fs::write(&outside, b"untouched").unwrap();
    for id in ["../../outside", "/tmp/outside", "A", "é", "a/b"] {
        assert!(store.chunk_path(id).starts_with(store.vault_dir()));
        assert!(!store.has_chunk(id, 9));
        assert!(store.read_chunk(id).is_err());
        assert!(store.write_chunk_tmp(id, b"bad").is_err());
        assert!(store.promote_tmp_chunk(&outside, id).is_err());
        assert!(store.quarantine_chunk(id).is_err());
        assert!(store.verify_chunk_on_disk(id, 9).is_err());
        assert!(store.sync_chunks(&[id.to_string()]).is_err());
    }
    assert_eq!(std::fs::read(&outside).unwrap(), b"untouched");
    assert!(store.promote_tmp_chunk(&outside, &"a".repeat(64)).is_err());
    assert_eq!(std::fs::read(&outside).unwrap(), b"untouched");
}

#[tokio::test]
async fn upload_persists_canonical_manifest_and_verifies_it() {
    let tmp = tempdir().unwrap();
    let vault = tmp.path().join("vault");
    let store = Store::new(&vault).unwrap();
    let db = Arc::new(Mutex::new(Database::open(vault.join("meta.db")).unwrap()));
    let router = create_router(AppState {
        store: store.clone(),
        db: db.clone(),
    });
    let data = b"canonical upload data";
    let id = sha256_hex(data);
    let manifest = Manifest::new(vec![file("a", &id, data.len() as u64)]).unwrap();
    let canonical = manifest.canonical_bytes().unwrap();
    let input = serde_json::to_vec_pretty(&manifest).unwrap();
    assert_ne!(input, canonical);
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/uploads")
                .body(Body::from(input))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let upload: UploadInitResponse = serde_json::from_slice(&body).unwrap();
    let manifest_id = manifest.manifest_id().unwrap();
    assert_eq!(
        db.lock()
            .unwrap()
            .get_manifest_bytes(&manifest_id)
            .unwrap()
            .unwrap(),
        canonical
    );

    let put = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/v1/uploads/{}/chunks/{id}", upload.upload_id))
                .body(Body::from(data.to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::CREATED);
    let commit = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/uploads/{}/commit", upload.upload_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(commit.status(), StatusCode::OK);
    let verified = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/verify")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(verified.status(), StatusCode::OK);
    let body = verified.into_body().collect().await.unwrap().to_bytes();
    assert!(
        serde_json::from_slice::<brokenvault::core::proto::VerifyReport>(&body)
            .unwrap()
            .healthy
    );
}
