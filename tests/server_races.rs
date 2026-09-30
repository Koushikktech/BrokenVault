use axum::body::Body;
use axum::http::{Request, StatusCode};
use brokenvault::core::hash::sha256_hex;
use brokenvault::core::manifest::{Manifest, ManifestEntry};
use brokenvault::server::api::{AppState, create_router};
use brokenvault::server::db::Database;
use brokenvault::server::store::Store;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;
use tower::ServiceExt;

#[tokio::test]
async fn closed_upload_cannot_accept_or_acknowledge_chunk() {
    let tmp = tempdir().unwrap();
    let vault = tmp.path().join("vault");
    let store = Store::new(&vault).unwrap();
    let db = Arc::new(Mutex::new(Database::open(vault.join("meta.db")).unwrap()));
    let data = b"stored data";
    let id = sha256_hex(data);
    let manifest = Manifest::new(vec![ManifestEntry::File {
        path: "file".to_string(),
        size: data.len() as u64,
        mtime: (0, 0),
        chunks: vec![(id.clone(), data.len() as u64)],
    }])
    .unwrap();
    let manifest_id = manifest.manifest_id().unwrap();
    {
        let db = db.lock().unwrap();
        db.store_manifest(&manifest_id, &manifest.canonical_bytes().unwrap())
            .unwrap();
        db.create_upload("closed", &manifest_id, data.len() as u64)
            .unwrap();
        db.create_upload("closed_with_chunk", &manifest_id, data.len() as u64)
            .unwrap();
        assert!(db.abort_upload("closed").unwrap());
    }
    let temp = store.write_chunk_tmp(&id, data).unwrap();
    store.promote_tmp_chunk(&temp, &id).unwrap();
    assert!(
        db.lock()
            .unwrap()
            .abort_upload("closed_with_chunk")
            .unwrap()
    );
    let router = create_router(AppState {
        store,
        db: db.clone(),
    });
    for upload in ["closed", "closed_with_chunk"] {
        let put = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/v1/uploads/{upload}/chunks/{id}"))
                    .body(Body::from(data.to_vec()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(put.status(), StatusCode::BAD_REQUEST);
        let db = db.lock().unwrap();
        assert!(
            !db.record_accepted_chunk(upload, &id, data.len() as u64)
                .unwrap()
        );
        assert_eq!(db.get_uploaded_bytes(upload).unwrap(), 0);
    }
}
