use crate::client::journal::{Journal, JournalEntry, make_journal_key};
use crate::client::scan::{LocalChunkDetail, scan_directory};
use crate::client::ui::{print_completed_summary, print_paused_summary};
use crate::core::errors::CoreError;
use crate::core::proto::{
    CommitConflictResponse, CommitResponse, UploadInitResponse, UploadStatusResponse,
};
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

pub struct UploadOptions {
    pub jobs: usize,
    pub stop_after_chunks: Option<usize>,
}

impl Default for UploadOptions {
    fn default() -> Self {
        Self {
            jobs: 8,
            stop_after_chunks: None,
        }
    }
}

pub fn run_backup(
    src_dir: impl AsRef<Path>,
    server_url: &str,
    options: UploadOptions,
) -> Result<Option<CommitResponse>, CoreError> {
    let server_url = server_url.trim_end_matches('/');
    let client = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .new_agent();

    let (manifest, chunk_details) = scan_directory(&src_dir)?;
    let manifest_bytes = manifest.canonical_bytes()?;
    let manifest_id = manifest.manifest_id()?;

    let mut journal = Journal::load_from_disk();
    let journal_key = make_journal_key(server_url, &manifest_id);

    if let Some(entry) = journal.get(&journal_key) {
        let status_url = format!("{}/v1/uploads/{}", server_url, entry.upload_id);
        if let Ok(mut res) = client.get(&status_url).call() {
            if res.status().as_u16() == 200 {
                if let Ok(status_dto) = res.body_mut().read_json::<UploadStatusResponse>() {
                    if status_dto.state == "committed" {
                        if let Some(version) = status_dto.version_id {
                            print_completed_summary(
                                &version,
                                manifest.total_bytes(),
                                0,
                                manifest.total_bytes(),
                            );
                            return Ok(None);
                        }
                    }
                }
            }
        }
    }

    let init_url = format!("{}/v1/uploads", server_url);
    let mut init_res = client
        .post(&init_url)
        .header("content-type", "application/json")
        .send(&manifest_bytes)
        .map_err(|e| CoreError::Io(std::io::Error::other(format!("upload init failed: {}", e))))?;

    let status_code = init_res.status().as_u16();
    if status_code != 200 && status_code != 201 {
        let body_str = init_res.body_mut().read_to_string().unwrap_or_default();
        return Err(CoreError::Io(std::io::Error::other(format!(
            "server rejected upload init (status {}): {}",
            status_code, body_str
        ))));
    }

    let init_dto: UploadInitResponse = init_res
        .body_mut()
        .read_json()
        .map_err(|e| CoreError::Io(std::io::Error::other(e)))?;

    journal.insert(
        journal_key.clone(),
        JournalEntry {
            upload_id: init_dto.upload_id.clone(),
            server_url: server_url.to_string(),
            manifest_id: manifest_id.clone(),
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(std::io::Error::other)?
                .as_secs() as i64,
            committed: false,
            version_id: None,
        },
    );
    let _ = journal.save_to_disk();

    let total_unique = manifest.unique_chunk_ids().len();
    let already_present = total_unique.saturating_sub(init_dto.missing.len());

    println!(
        "Upload {} ({}) — server already has {} / {} chunks",
        init_dto.upload_id,
        if init_dto.resumed { "resumed" } else { "new" },
        already_present,
        total_unique
    );

    let mut chunk_map: HashMap<String, LocalChunkDetail> = HashMap::new();
    for detail in chunk_details {
        chunk_map.insert(detail.chunk_id.clone(), detail);
    }

    let interrupted = Arc::new(AtomicBool::new(false));
    let interrupted_ctrlc = interrupted.clone();
    let _ = ctrlc::set_handler(move || {
        interrupted_ctrlc.store(true, Ordering::SeqCst);
    });

    let uploaded_count = Arc::new(AtomicUsize::new(0));

    let upload_chunks = |chunks_to_send: &[String]| -> Result<(), CoreError> {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(options.jobs)
            .build()
            .map_err(std::io::Error::other)?;

        pool.install(|| {
            chunks_to_send
                .par_iter()
                .try_for_each(|chunk_id| -> Result<(), CoreError> {
                    if interrupted.load(Ordering::SeqCst) {
                        return Ok(());
                    }

                    if let Some(limit) = options.stop_after_chunks {
                        if uploaded_count.load(Ordering::SeqCst) >= limit {
                            interrupted.store(true, Ordering::SeqCst);
                            return Ok(());
                        }
                    }

                    let detail = chunk_map.get(chunk_id).ok_or_else(|| {
                        CoreError::Io(std::io::Error::other(format!(
                            "local chunk detail missing: {}",
                            chunk_id
                        )))
                    })?;

                    let mut file = File::open(&detail.source_path)?;
                    file.seek(SeekFrom::Start(detail.offset))?;
                    let mut chunk_data = vec![0u8; detail.len as usize];
                    file.read_exact(&mut chunk_data)?;

                    let put_url = format!(
                        "{}/v1/uploads/{}/chunks/{}",
                        server_url, init_dto.upload_id, chunk_id
                    );

                    let put_res = client
                        .put(&put_url)
                        .header("content-type", "application/octet-stream")
                        .send(&chunk_data)
                        .map_err(|e| {
                            CoreError::Io(std::io::Error::other(format!(
                                "chunk upload failed: {}",
                                e
                            )))
                        })?;

                    let put_status = put_res.status().as_u16();
                    if put_status != 200 && put_status != 201 {
                        return Err(CoreError::Io(std::io::Error::other(format!(
                            "chunk upload returned status {}: {}",
                            put_status, chunk_id
                        ))));
                    }

                    uploaded_count.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
        })?;

        Ok(())
    };

    upload_chunks(&init_dto.missing)?;

    if interrupted.load(Ordering::SeqCst) {
        let present_now = already_present + uploaded_count.load(Ordering::SeqCst);
        print_paused_summary(&init_dto.upload_id, present_now, total_unique);
        return Err(CoreError::Interrupted);
    }

    let mut commit_attempts = 0;
    let commit_url = format!("{}/v1/uploads/{}/commit", server_url, init_dto.upload_id);

    loop {
        commit_attempts += 1;
        let mut commit_res = client.post(&commit_url).send(&[]).map_err(|e| {
            CoreError::Io(std::io::Error::other(format!(
                "commit request failed: {}",
                e
            )))
        })?;

        let status = commit_res.status().as_u16();
        if status == 200 {
            let commit_dto: CommitResponse = commit_res
                .body_mut()
                .read_json()
                .map_err(|e| CoreError::Io(std::io::Error::other(e)))?;

            print_completed_summary(
                &commit_dto.version,
                commit_dto.total_bytes,
                commit_dto.uploaded_bytes,
                commit_dto.reused_bytes,
            );

            journal.insert(
                journal_key,
                JournalEntry {
                    upload_id: init_dto.upload_id,
                    server_url: server_url.to_string(),
                    manifest_id,
                    created_at: commit_dto.committed_at,
                    committed: true,
                    version_id: Some(commit_dto.version.clone()),
                },
            );
            let _ = journal.save_to_disk();

            return Ok(Some(commit_dto));
        } else if status == 409 && commit_attempts <= 3 {
            let conflict: CommitConflictResponse = commit_res
                .body_mut()
                .read_json()
                .map_err(|e| CoreError::Io(std::io::Error::other(e)))?;
            eprintln!(
                "Notice: server requested re-upload of {} missing/corrupt chunks (retry {})",
                conflict.missing_or_corrupt.len(),
                commit_attempts
            );
            upload_chunks(&conflict.missing_or_corrupt)?;
        } else {
            let body = commit_res.body_mut().read_to_string().unwrap_or_default();
            return Err(CoreError::Io(std::io::Error::other(format!(
                "commit failed with status {}: {}",
                status, body
            ))));
        }
    }
}
