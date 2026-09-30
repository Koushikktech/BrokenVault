use crate::core::errors::CoreError;
use crate::core::hash::sha256_hex;
use crate::core::manifest::{Manifest, ManifestEntry};
use crate::core::pathsafe::resolve_under_root;
use rayon::prelude::*;
use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct RestoreOptions {
    pub jobs: usize,
}

impl Default for RestoreOptions {
    fn default() -> Self {
        Self { jobs: 8 }
    }
}

struct PlannedChunk {
    chunk_id: String,
    target_path: PathBuf,
    offset: u64,
    len: u64,
}

pub fn run_restore(
    version_id: &str,
    dest: impl AsRef<Path>,
    server_url: &str,
    options: RestoreOptions,
) -> Result<(), CoreError> {
    let dest = dest.as_ref();
    let server_url = server_url.trim_end_matches('/');
    let client = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .new_agent();

    check_destination_clean(dest)?;

    let manifest_url = format!("{}/v1/versions/{}/manifest", server_url, version_id);
    let mut manifest_res = client.get(&manifest_url).call().map_err(|e| {
        CoreError::Io(std::io::Error::other(format!(
            "failed to fetch manifest: {}",
            e
        )))
    })?;

    if manifest_res.status().as_u16() != 200 {
        return Err(CoreError::Io(std::io::Error::other(format!(
            "version {} manifest not found (status {})",
            version_id,
            manifest_res.status()
        ))));
    }

    let manifest_bytes = manifest_res
        .body_mut()
        .read_to_vec()
        .map_err(|e| CoreError::Io(std::io::Error::other(e)))?;
    let manifest = Manifest::from_bytes(&manifest_bytes)?;

    let execution_result = execute_restore_plan(&manifest, dest, server_url, &options, &client);

    if execution_result.is_err() {
        cleanup_destination(dest);
    }

    execution_result
}

fn check_destination_clean(dest: &Path) -> Result<(), CoreError> {
    if dest.exists() {
        let is_empty = fs::read_dir(dest)
            .map_err(std::io::Error::other)?
            .next()
            .is_none();
        if !is_empty {
            return Err(CoreError::Io(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!(
                    "destination directory exists and is not empty: {}",
                    dest.display()
                ),
            )));
        }
    } else {
        fs::create_dir_all(dest)?;
    }
    Ok(())
}

fn cleanup_destination(dest: &Path) {
    if dest.exists() {
        if let Ok(entries) = fs::read_dir(dest) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let _ = fs::remove_dir_all(path);
                } else {
                    let _ = fs::remove_file(path);
                }
            }
        }
    }
}

fn execute_restore_plan(
    manifest: &Manifest,
    dest: &Path,
    server_url: &str,
    options: &RestoreOptions,
    client: &ureq::Agent,
) -> Result<(), CoreError> {
    let mut planned_chunks = Vec::new();
    let mut dirs_to_create = Vec::new();
    let mut file_mtimes = Vec::new();
    let mut dir_mtimes = Vec::new();

    for entry in &manifest.entries {
        match entry {
            ManifestEntry::Dir { path, mtime } => {
                let target = resolve_under_root(dest, path)?;
                dirs_to_create.push(target.clone());
                dir_mtimes.push((target, *mtime));
            }
            ManifestEntry::File {
                path,
                size,
                mtime,
                chunks,
            } => {
                let target = resolve_under_root(dest, path)?;
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }

                let file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&target)
                    .map_err(|e| {
                        CoreError::Io(std::io::Error::other(format!(
                            "failed to create file {}: {}",
                            target.display(),
                            e
                        )))
                    })?;

                file.set_len(*size)?;
                file_mtimes.push((target.clone(), *mtime));

                let mut offset = 0;
                for (chunk_id, len) in chunks {
                    planned_chunks.push(PlannedChunk {
                        chunk_id: chunk_id.clone(),
                        target_path: target.clone(),
                        offset,
                        len: *len,
                    });
                    offset += *len;
                }
            }
        }
    }

    for dir_path in dirs_to_create {
        fs::create_dir_all(&dir_path)?;
    }

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(options.jobs)
        .build()
        .map_err(std::io::Error::other)?;

    let aborted = Arc::new(AtomicBool::new(false));

    pool.install(|| {
        planned_chunks
            .par_iter()
            .try_for_each(|item| -> Result<(), CoreError> {
                if aborted.load(Ordering::SeqCst) {
                    return Err(CoreError::Io(std::io::Error::other("restore aborted")));
                }

                let chunk_url = format!("{}/v1/chunks/{}", server_url, item.chunk_id);
                let mut chunk_res = client.get(&chunk_url).call().map_err(|e| {
                    aborted.store(true, Ordering::SeqCst);
                    CoreError::Io(std::io::Error::other(format!(
                        "failed to download chunk {}: {}",
                        item.chunk_id, e
                    )))
                })?;

                let status = chunk_res.status().as_u16();
                if status != 200 {
                    aborted.store(true, Ordering::SeqCst);
                    return Err(CoreError::Io(std::io::Error::other(format!(
                        "chunk download failed with status {}: {}",
                        status, item.chunk_id
                    ))));
                }

                let chunk_data = chunk_res.body_mut().read_to_vec().map_err(|e| {
                    aborted.store(true, Ordering::SeqCst);
                    CoreError::Io(std::io::Error::other(e))
                })?;

                if chunk_data.len() as u64 != item.len {
                    aborted.store(true, Ordering::SeqCst);
                    return Err(CoreError::Io(std::io::Error::other(format!(
                        "chunk size mismatch for {}: expected {}, got {}",
                        item.chunk_id,
                        item.len,
                        chunk_data.len()
                    ))));
                }

                let computed_hash = sha256_hex(&chunk_data);
                if computed_hash != item.chunk_id {
                    aborted.store(true, Ordering::SeqCst);
                    return Err(CoreError::Io(std::io::Error::other(format!(
                        "chunk hash verification failed for {}: got {}",
                        item.chunk_id, computed_hash
                    ))));
                }

                let mut file = OpenOptions::new()
                    .write(true)
                    .open(&item.target_path)
                    .map_err(|e| {
                        aborted.store(true, Ordering::SeqCst);
                        CoreError::Io(std::io::Error::other(e))
                    })?;

                file.seek(SeekFrom::Start(item.offset))?;
                file.write_all(&chunk_data)?;
                file.flush()?;

                Ok(())
            })
    })?;

    for (file_path, (secs, nsecs)) in file_mtimes {
        let ft = filetime::FileTime::from_unix_time(secs, nsecs);
        let _ = filetime::set_file_times(&file_path, ft, ft);
    }

    dir_mtimes.sort_by(|a, b| b.0.cmp(&a.0));
    for (dir_path, (secs, nsecs)) in dir_mtimes {
        let ft = filetime::FileTime::from_unix_time(secs, nsecs);
        let _ = filetime::set_file_times(&dir_path, ft, ft);
    }

    println!("✔ Restore completed successfully into {}", dest.display());
    Ok(())
}
