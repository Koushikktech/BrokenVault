use crate::core::errors::CoreError;
use crate::core::hash::sha256_hex;
use crate::core::manifest::{Manifest, ManifestEntry};
use crate::core::proto::{
    ApiError, CommitConflictResponse, CommitResponse, HealthResponse, OpenUploadSummary,
    UploadInitResponse, UploadStatusResponse, VersionSummary,
};
use crate::server::commit::{CommitOutcome, execute_commit};
use crate::server::db::{Database, VersionRow};
use crate::server::store::Store;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    pub db: Arc<Mutex<Database>>,
}

pub fn create_router(state: AppState) -> Router {
    Router::new()
        .route("/v1/health", get(health_handler))
        .route(
            "/v1/uploads",
            post(create_or_resume_upload).get(list_uploads),
        )
        .route(
            "/v1/uploads/{id}",
            get(get_upload_handler).delete(abort_upload_handler),
        )
        .route("/v1/uploads/{id}/chunks/{chunk_id}", put(put_chunk_handler))
        .route("/v1/uploads/{id}/commit", post(commit_upload_handler))
        .route("/v1/versions", get(list_versions_handler))
        .route("/v1/versions/{version_id}", get(get_version_handler))
        .route(
            "/v1/versions/{version_id}/manifest",
            get(get_version_manifest_handler),
        )
        .route("/v1/chunks/{chunk_id}", get(get_chunk_handler))
        .route("/v1/verify", post(verify_handler))
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
        .with_state(state)
}

async fn health_handler(State(state): State<AppState>) -> impl IntoResponse {
    let vault_id = {
        let db = match state.db.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        match db.vault_id() {
            Ok(id) => id,
            Err(_) => "unknown".to_string(),
        }
    };

    Json(HealthResponse {
        vault_id,
        protocol_version: "1.0".to_string(),
        status: "ok".to_string(),
    })
}

async fn create_or_resume_upload(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, (StatusCode, Json<ApiError>)> {
    let manifest = Manifest::from_bytes(&body).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(ApiError::new("INVALID_MANIFEST", e.to_string(), None)),
        )
    })?;

    let canonical_bytes = manifest.canonical_bytes().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError::new("HASH_ERROR", e.to_string(), None)),
        )
    })?;
    let manifest_id = sha256_hex(&canonical_bytes);

    let mut chunk_map = HashMap::new();
    for entry in &manifest.entries {
        if let ManifestEntry::File { chunks, .. } = entry {
            for (id, len) in chunks {
                chunk_map.insert(id.clone(), *len);
            }
        }
    }

    let mut missing = Vec::new();
    for (id, len) in &chunk_map {
        if !state.store.has_chunk(id, *len) {
            missing.push(id.clone());
        }
    }

    let (upload_id, resumed, is_new) = {
        let db = state.db.lock().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError::new("LOCK_ERROR", "db lock error", None)),
            )
        })?;

        db.store_manifest(&manifest_id, &canonical_bytes)
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ApiError::new("DB_ERROR", e.to_string(), None)),
                )
            })?;

        if let Some(existing) = db.find_open_upload_by_manifest(&manifest_id).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError::new("DB_ERROR", e.to_string(), None)),
            )
        })? {
            (existing.id, true, false)
        } else {
            let rand_val: u64 = rand::random();
            let new_id = format!("upl_{:016x}", rand_val);
            db.create_upload(&new_id, &manifest_id, manifest.total_bytes())
                .map_err(|e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(ApiError::new("DB_ERROR", e.to_string(), None)),
                    )
                })?;
            (new_id, false, true)
        }
    };

    let resp = UploadInitResponse {
        upload_id,
        resumed,
        total_bytes: manifest.total_bytes(),
        chunks_total: chunk_map.len(),
        missing,
    };

    let status = if is_new {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(resp)).into_response())
}

async fn list_uploads(
    State(state): State<AppState>,
) -> Result<Json<Vec<OpenUploadSummary>>, (StatusCode, Json<ApiError>)> {
    let rows = {
        let db = state.db.lock().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError::new("LOCK_ERROR", "db lock error", None)),
            )
        })?;
        db.list_open_uploads().map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError::new("DB_ERROR", e.to_string(), None)),
            )
        })?
    };

    let mut summaries = Vec::new();
    for row in rows {
        let manifest_bytes = {
            let db = state.db.lock().map_err(|_| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ApiError::new("LOCK_ERROR", "db lock error", None)),
                )
            })?;
            db.get_manifest_bytes(&row.manifest_id).map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ApiError::new("DB_ERROR", e.to_string(), None)),
                )
            })?
        };

        let (total_chunks, present_chunks) = if let Some(bytes) = manifest_bytes {
            if let Ok(manifest) = Manifest::from_bytes(&bytes) {
                let mut unique_chunks = HashMap::new();
                for entry in &manifest.entries {
                    if let ManifestEntry::File { chunks, .. } = entry {
                        for (id, len) in chunks {
                            unique_chunks.insert(id.clone(), *len);
                        }
                    }
                }
                let present = unique_chunks
                    .iter()
                    .filter(|(id, len)| state.store.has_chunk(id, **len))
                    .count();
                (unique_chunks.len(), present)
            } else {
                (0, 0)
            }
        } else {
            (0, 0)
        };

        summaries.push(OpenUploadSummary {
            upload_id: row.id,
            total_bytes: row.total_bytes,
            created_at: row.created_at,
            state: row.state,
            chunks_total: total_chunks,
            chunks_present: present_chunks,
        });
    }

    Ok(Json(summaries))
}

async fn get_upload_handler(
    State(state): State<AppState>,
    Path(upload_id): Path<String>,
) -> Result<Json<UploadStatusResponse>, (StatusCode, Json<ApiError>)> {
    let upload = {
        let db = state.db.lock().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError::new("LOCK_ERROR", "db lock error", None)),
            )
        })?;
        db.get_upload(&upload_id)
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ApiError::new("DB_ERROR", e.to_string(), None)),
                )
            })?
            .ok_or_else(|| {
                (
                    StatusCode::NOT_FOUND,
                    Json(ApiError::new(
                        "UPLOAD_NOT_FOUND",
                        format!("upload {} not found", upload_id),
                        None,
                    )),
                )
            })?
    };

    if upload.state == "committed" {
        return Ok(Json(UploadStatusResponse {
            upload_id: upload.id,
            state: "committed".to_string(),
            total_bytes: upload.total_bytes,
            chunks_total: 0,
            missing: Vec::new(),
            version_id: upload.version_id,
        }));
    }

    let manifest_bytes = {
        let db = state.db.lock().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError::new("LOCK_ERROR", "db lock error", None)),
            )
        })?;
        db.get_manifest_bytes(&upload.manifest_id)
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ApiError::new("DB_ERROR", e.to_string(), None)),
                )
            })?
            .ok_or_else(|| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ApiError::new(
                        "MANIFEST_NOT_FOUND",
                        "manifest not found",
                        None,
                    )),
                )
            })?
    };

    let manifest = Manifest::from_bytes(&manifest_bytes).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError::new("INVALID_MANIFEST", e.to_string(), None)),
        )
    })?;

    let mut chunk_map = HashMap::new();
    for entry in &manifest.entries {
        if let ManifestEntry::File { chunks, .. } = entry {
            for (id, len) in chunks {
                chunk_map.insert(id.clone(), *len);
            }
        }
    }

    let mut missing = Vec::new();
    for (id, len) in &chunk_map {
        if !state.store.has_chunk(id, *len) {
            missing.push(id.clone());
        }
    }

    Ok(Json(UploadStatusResponse {
        upload_id: upload.id,
        state: upload.state,
        total_bytes: upload.total_bytes,
        chunks_total: chunk_map.len(),
        missing,
        version_id: None,
    }))
}

async fn abort_upload_handler(
    State(state): State<AppState>,
    Path(upload_id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    let db = state.db.lock().map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError::new("LOCK_ERROR", "db lock error", None)),
        )
    })?;

    let aborted = db.abort_upload(&upload_id).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError::new("DB_ERROR", e.to_string(), None)),
        )
    })?;

    if aborted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((
            StatusCode::NOT_FOUND,
            Json(ApiError::new(
                "NOT_FOUND_OR_CLOSED",
                format!("upload {} not found or already closed", upload_id),
                None,
            )),
        ))
    }
}

async fn put_chunk_handler(
    State(state): State<AppState>,
    Path((upload_id, chunk_id)): Path<(String, String)>,
    body: Bytes,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    let (manifest_bytes, is_open) = {
        let db = state.db.lock().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError::new("LOCK_ERROR", "db lock error", None)),
            )
        })?;
        let upload = db
            .get_upload(&upload_id)
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ApiError::new("DB_ERROR", e.to_string(), None)),
                )
            })?
            .ok_or_else(|| {
                (
                    StatusCode::NOT_FOUND,
                    Json(ApiError::new(
                        "UPLOAD_NOT_FOUND",
                        format!("upload {} not found", upload_id),
                        None,
                    )),
                )
            })?;

        let bytes = db
            .get_manifest_bytes(&upload.manifest_id)
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ApiError::new("DB_ERROR", e.to_string(), None)),
                )
            })?
            .ok_or_else(|| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ApiError::new(
                        "MANIFEST_NOT_FOUND",
                        "manifest not found",
                        None,
                    )),
                )
            })?;

        (bytes, upload.state == "open")
    };

    if !is_open {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError::new(
                "UPLOAD_NOT_OPEN",
                format!("upload {} is not in open state", upload_id),
                None,
            )),
        ));
    }

    let manifest = Manifest::from_bytes(&manifest_bytes).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError::new("INVALID_MANIFEST", e.to_string(), None)),
        )
    })?;

    let expected_len = {
        let mut found = None;
        for entry in &manifest.entries {
            if let ManifestEntry::File { chunks, .. } = entry {
                for (id, len) in chunks {
                    if id == &chunk_id {
                        found = Some(*len);
                        break;
                    }
                }
            }
            if found.is_some() {
                break;
            }
        }
        found.ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(ApiError::new(
                    "NOT_IN_MANIFEST",
                    format!("chunk {} not part of manifest", chunk_id),
                    None,
                )),
            )
        })?
    };

    if (body.len() as u64) != expected_len {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ApiError::new(
                "SIZE_MISMATCH",
                format!(
                    "body length {} does not match manifest length {}",
                    body.len(),
                    expected_len
                ),
                None,
            )),
        ));
    }

    let actual_hash = sha256_hex(&body);
    if actual_hash != chunk_id {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ApiError::new(
                "HASH_MISMATCH",
                format!(
                    "computed hash {} does not match requested chunk id {}",
                    actual_hash, chunk_id
                ),
                None,
            )),
        ));
    }

    if state.store.has_chunk(&chunk_id, expected_len) {
        let db = state.db.lock().map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError::new("LOCK_ERROR", "db lock error", None)),
            )
        })?;
        if db.is_upload_open(&upload_id).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError::new("DB_ERROR", e.to_string(), None)),
            )
        })? {
            return Ok(StatusCode::OK);
        }
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError::new(
                "UPLOAD_NOT_OPEN",
                format!("upload {} is not in open state", upload_id),
                None,
            )),
        ));
    }

    let store = state.store.clone();
    let body_vec = body.to_vec();
    let c_id = chunk_id.clone();
    let db_arc = state.db.clone();
    let u_id = upload_id.clone();

    let accepted = tokio::task::spawn_blocking(move || -> Result<bool, CoreError> {
        let tmp_path = store.write_chunk_tmp(&c_id, &body_vec)?;
        let db = db_arc
            .lock()
            .map_err(|_| std::io::Error::other("db lock error"))?;
        if !db.is_upload_open(&u_id)? {
            let _ = std::fs::remove_file(&tmp_path);
            return Ok(false);
        }
        store.promote_tmp_chunk(&tmp_path, &c_id)?;
        db.record_accepted_chunk(&u_id, &c_id, expected_len)
    })
    .await
    .map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError::new(
                "ASYNC_ERROR",
                "blocking task join error",
                None,
            )),
        )
    })?
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError::new("STORE_ERROR", e.to_string(), None)),
        )
    })?;

    if !accepted {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError::new(
                "UPLOAD_NOT_OPEN",
                format!("upload {} is not in open state", upload_id),
                None,
            )),
        ));
    }
    Ok(StatusCode::CREATED)
}

async fn commit_upload_handler(
    State(state): State<AppState>,
    Path(upload_id): Path<String>,
) -> Result<Response, (StatusCode, Json<ApiError>)> {
    let store = state.store.clone();
    let db_arc = state.db.clone();
    let u_id = upload_id.clone();

    let outcome = tokio::task::spawn_blocking(move || {
        let mut db = match db_arc.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        execute_commit(&store, &mut db, &u_id)
    })
    .await
    .map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError::new(
                "ASYNC_ERROR",
                "blocking task join error",
                None,
            )),
        )
    })?
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError::new("COMMIT_ERROR", e.to_string(), None)),
        )
    })?;

    match outcome {
        CommitOutcome::Success(version) | CommitOutcome::AlreadyCommitted(version) => {
            let resp = CommitResponse {
                version: version.id,
                committed_at: version.committed_at,
                total_bytes: version.total_bytes,
                uploaded_bytes: version.uploaded_bytes,
                reused_bytes: version.reused_bytes,
                files: version.files,
                dirs: version.dirs,
                chunks: version.chunks,
            };
            Ok((StatusCode::OK, Json(resp)).into_response())
        }
        CommitOutcome::Conflict { missing_or_corrupt } => {
            let resp = CommitConflictResponse { missing_or_corrupt };
            Ok((StatusCode::CONFLICT, Json(resp)).into_response())
        }
        CommitOutcome::Aborted => Err((
            StatusCode::GONE,
            Json(ApiError::new(
                "UPLOAD_ABORTED",
                format!("upload {} was aborted", upload_id),
                None,
            )),
        )),
    }
}

async fn list_versions_handler(
    State(state): State<AppState>,
) -> Result<Json<Vec<VersionSummary>>, (StatusCode, Json<ApiError>)> {
    let db = state.db.lock().map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError::new("LOCK_ERROR", "db lock error", None)),
        )
    })?;

    let versions = db.list_versions().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError::new("DB_ERROR", e.to_string(), None)),
        )
    })?;

    let summaries = versions
        .into_iter()
        .map(|v: VersionRow| VersionSummary {
            id: v.id,
            upload_id: v.upload_id,
            manifest_id: v.manifest_id,
            committed_at: v.committed_at,
            total_bytes: v.total_bytes,
            uploaded_bytes: v.uploaded_bytes,
            reused_bytes: v.reused_bytes,
            files: v.files,
            dirs: v.dirs,
            chunks: v.chunks,
        })
        .collect();

    Ok(Json(summaries))
}

async fn get_version_handler(
    State(state): State<AppState>,
    Path(version_id): Path<String>,
) -> Result<Json<VersionSummary>, (StatusCode, Json<ApiError>)> {
    let db = state.db.lock().map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError::new("LOCK_ERROR", "db lock error", None)),
        )
    })?;

    let version = db
        .get_version(&version_id)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError::new("DB_ERROR", e.to_string(), None)),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ApiError::new(
                    "VERSION_NOT_FOUND",
                    format!("version {} not found", version_id),
                    None,
                )),
            )
        })?;

    Ok(Json(VersionSummary {
        id: version.id,
        upload_id: version.upload_id,
        manifest_id: version.manifest_id,
        committed_at: version.committed_at,
        total_bytes: version.total_bytes,
        uploaded_bytes: version.uploaded_bytes,
        reused_bytes: version.reused_bytes,
        files: version.files,
        dirs: version.dirs,
        chunks: version.chunks,
    }))
}

async fn get_version_manifest_handler(
    State(state): State<AppState>,
    Path(version_id): Path<String>,
) -> Result<Response, (StatusCode, Json<ApiError>)> {
    let db = state.db.lock().map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError::new("LOCK_ERROR", "db lock error", None)),
        )
    })?;

    let bytes = db
        .get_version_manifest(&version_id)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError::new("DB_ERROR", e.to_string(), None)),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ApiError::new(
                    "VERSION_NOT_FOUND",
                    format!("version {} not found", version_id),
                    None,
                )),
            )
        })?;

    Ok((
        StatusCode::OK,
        [("content-type", "application/json")],
        bytes,
    )
        .into_response())
}

async fn get_chunk_handler(
    State(state): State<AppState>,
    Path(chunk_id): Path<String>,
) -> Result<Response, (StatusCode, Json<ApiError>)> {
    let data = state.store.read_chunk(&chunk_id).map_err(|_| {
        (
            StatusCode::NOT_FOUND,
            Json(ApiError::new(
                "CHUNK_NOT_FOUND",
                format!("chunk {} not found", chunk_id),
                None,
            )),
        )
    })?;

    let computed = sha256_hex(&data);
    if computed != chunk_id {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError::new(
                "CHUNK_CORRUPT",
                format!("chunk {} failed server-side integrity check", chunk_id),
                None,
            )),
        ));
    }

    Ok((
        StatusCode::OK,
        [("content-type", "application/octet-stream")],
        data,
    )
        .into_response())
}

async fn verify_handler(
    State(state): State<AppState>,
) -> Result<Json<crate::core::proto::VerifyReport>, (StatusCode, Json<ApiError>)> {
    let store = state.store.clone();
    let db_arc = state.db.clone();

    let report = tokio::task::spawn_blocking(move || {
        let db = match db_arc.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        crate::server::verify::execute_verification(&store, &db)
    })
    .await
    .map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError::new(
                "ASYNC_ERROR",
                "blocking task join error",
                None,
            )),
        )
    })?
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiError::new("VERIFY_ERROR", e.to_string(), None)),
        )
    })?;

    Ok(Json(report))
}
