use crate::core::errors::CoreError;
use crate::core::manifest::{Manifest, ManifestEntry};
use crate::server::db::{Database, VersionRow};
use crate::server::store::{ChunkVerificationError, Store};
use rayon::prelude::*;
use std::collections::HashMap;

#[derive(Debug)]
pub enum CommitOutcome {
    Success(VersionRow),
    AlreadyCommitted(VersionRow),
    Conflict { missing_or_corrupt: Vec<String> },
    Aborted,
}

pub fn execute_commit(
    store: &Store,
    db: &mut Database,
    upload_id: &str,
) -> Result<CommitOutcome, CoreError> {
    let upload = match db.get_upload(upload_id)? {
        Some(u) => u,
        None => {
            return Err(CoreError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("upload not found: {}", upload_id),
            )));
        }
    };

    if upload.state == "committed" {
        if let Some(v_id) = &upload.version_id {
            if let Some(version) = db.get_version(v_id)? {
                return Ok(CommitOutcome::AlreadyCommitted(version));
            }
        }
        return Err(CoreError::Io(std::io::Error::other(format!(
            "upload {} is marked committed but version row missing",
            upload_id
        ))));
    }

    if upload.state == "aborted" {
        return Ok(CommitOutcome::Aborted);
    }

    let manifest_bytes = match db.get_manifest_bytes(&upload.manifest_id)? {
        Some(b) => b,
        None => {
            return Err(CoreError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "manifest {} not found for upload {}",
                    upload.manifest_id, upload_id
                ),
            )));
        }
    };

    let manifest = Manifest::from_bytes(&manifest_bytes)?;

    let mut chunk_specs = HashMap::new();
    for entry in &manifest.entries {
        if let ManifestEntry::File { chunks, .. } = entry {
            for (id, len) in chunks {
                chunk_specs.insert(id.clone(), *len);
            }
        }
    }

    let chunk_specs_vec: Vec<(String, u64)> = chunk_specs.into_iter().collect();

    let check_results: Vec<(String, Result<(), ChunkVerificationError>)> = chunk_specs_vec
        .par_iter()
        .map(|(chunk_id, expected_len)| {
            let res = store.verify_chunk_on_disk(chunk_id, *expected_len);
            (chunk_id.clone(), res)
        })
        .collect();

    let mut missing_or_corrupt = Vec::new();
    for (chunk_id, res) in check_results {
        match res {
            Ok(()) => {}
            Err(ChunkVerificationError::Missing) => {
                missing_or_corrupt.push(chunk_id);
            }
            Err(ChunkVerificationError::SizeMismatch { .. })
            | Err(ChunkVerificationError::HashMismatch { .. }) => {
                let _ = store.quarantine_chunk(&chunk_id);
                missing_or_corrupt.push(chunk_id);
            }
        }
    }

    if !missing_or_corrupt.is_empty() {
        return Ok(CommitOutcome::Conflict { missing_or_corrupt });
    }

    let all_chunk_ids: Vec<String> = manifest.unique_chunk_ids();
    store.sync_chunks(&all_chunk_ids)?;

    let version = db.commit_transaction(
        upload_id,
        &upload.manifest_id,
        manifest.total_bytes(),
        manifest.total_files(),
        manifest.total_dirs(),
        manifest.total_chunks(),
    )?;

    Ok(CommitOutcome::Success(version))
}
