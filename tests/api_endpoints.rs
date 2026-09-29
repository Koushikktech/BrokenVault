use axum::body::Body;
use axum::http::{Request, StatusCode};
use brokenvault::core::chunker::chunk_slice;
use brokenvault::core::manifest::{Manifest, ManifestEntry};
use brokenvault::core::proto::{
    CommitResponse, HealthResponse, UploadInitResponse, VersionSummary,
};
use brokenvault::server::api::{AppState, create_router};
use brokenvault::server::db::Database;
use brokenvault::server::debug::{DamageMode, apply_damage};
use brokenvault::server::store::Store;
use http_body_util::BodyExt;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;
use tower::ServiceExt;

#[tokio::test]
async fn test_http_api_in_memory_lifecycle() {
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
    let health_body = health_res.into_body().collect().await.unwrap().to_bytes();
    let health_dto: HealthResponse = serde_json::from_slice(&health_body).unwrap();
    assert_eq!(health_dto.status, "ok");
    assert!(health_dto.vault_id.starts_with("vlt_"));

    let data = b"hello brokenvault in-memory http test bytes";
    let chunks = chunk_slice(data).unwrap();
    let chunk_id = &chunks[0].hash;
    let chunk_len = chunks[0].len;

    let entries = vec![ManifestEntry::File {
        path: "hello.txt".to_string(),
        size: chunk_len,
        mtime: (1700000000, 0),
        chunks: vec![(chunk_id.clone(), chunk_len)],
    }];
    let manifest = Manifest::new(entries).unwrap();
    let manifest_bytes = manifest.canonical_bytes().unwrap();

    let init_req = Request::builder()
        .uri("/v1/uploads")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(manifest_bytes))
        .unwrap();
    let init_res = router.clone().oneshot(init_req).await.unwrap();
    assert_eq!(init_res.status(), StatusCode::CREATED);
    let init_body = init_res.into_body().collect().await.unwrap().to_bytes();
    let init_dto: UploadInitResponse = serde_json::from_slice(&init_body).unwrap();
    assert_eq!(init_dto.missing, vec![chunk_id.clone()]);
    assert!(!init_dto.resumed);

    let bad_put_req = Request::builder()
        .uri(format!(
            "/v1/uploads/{}/chunks/{}",
            init_dto.upload_id, chunk_id
        ))
        .method("PUT")
        .body(Body::from("corrupt tampered bytes"))
        .unwrap();
    let bad_put_res = router.clone().oneshot(bad_put_req).await.unwrap();
    assert_eq!(bad_put_res.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let put_req1 = Request::builder()
        .uri(format!(
            "/v1/uploads/{}/chunks/{}",
            init_dto.upload_id, chunk_id
        ))
        .method("PUT")
        .body(Body::from(data.to_vec()))
        .unwrap();
    let put_res1 = router.clone().oneshot(put_req1).await.unwrap();
    assert_eq!(put_res1.status(), StatusCode::CREATED);

    let put_req2 = Request::builder()
        .uri(format!(
            "/v1/uploads/{}/chunks/{}",
            init_dto.upload_id, chunk_id
        ))
        .method("PUT")
        .body(Body::from(data.to_vec()))
        .unwrap();
    let put_res2 = router.clone().oneshot(put_req2).await.unwrap();
    assert_eq!(put_res2.status(), StatusCode::OK);

    let commit_req = Request::builder()
        .uri(format!("/v1/uploads/{}/commit", init_dto.upload_id))
        .method("POST")
        .body(Body::empty())
        .unwrap();
    let commit_res = router.clone().oneshot(commit_req).await.unwrap();
    assert_eq!(commit_res.status(), StatusCode::OK);
    let commit_body = commit_res.into_body().collect().await.unwrap().to_bytes();
    let commit_dto: CommitResponse = serde_json::from_slice(&commit_body).unwrap();
    assert_eq!(commit_dto.version, "v1");
    assert_eq!(commit_dto.uploaded_bytes, chunk_len);
    assert_eq!(commit_dto.reused_bytes, 0);

    let versions_req = Request::builder()
        .uri("/v1/versions")
        .method("GET")
        .body(Body::empty())
        .unwrap();
    let versions_res = router.clone().oneshot(versions_req).await.unwrap();
    assert_eq!(versions_res.status(), StatusCode::OK);
    let versions_body = versions_res.into_body().collect().await.unwrap().to_bytes();
    let versions_dto: Vec<VersionSummary> = serde_json::from_slice(&versions_body).unwrap();
    assert_eq!(versions_dto.len(), 1);
    assert_eq!(versions_dto[0].id, "v1");

    let chunk_req = Request::builder()
        .uri(format!("/v1/chunks/{}", chunk_id))
        .method("GET")
        .body(Body::empty())
        .unwrap();
    let chunk_res = router.clone().oneshot(chunk_req).await.unwrap();
    assert_eq!(chunk_res.status(), StatusCode::OK);
    let chunk_body = chunk_res.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&chunk_body[..], data);

    apply_damage(&store, DamageMode::Flip, Some(chunk_id)).unwrap();

    let corrupt_req = Request::builder()
        .uri(format!("/v1/chunks/{}", chunk_id))
        .method("GET")
        .body(Body::empty())
        .unwrap();
    let corrupt_res = router.clone().oneshot(corrupt_req).await.unwrap();
    assert_eq!(corrupt_res.status(), StatusCode::INTERNAL_SERVER_ERROR);
}
