use crate::core::errors::CoreError;
use crate::core::hash::sha256_hex;
use crate::core::manifest::{Manifest, ManifestEntry};
use crate::core::pathsafe::resolve_under_root;
use rayon::prelude::*;
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(unix)]
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};

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
    file: Arc<File>,
    offset: u64,
    len: u64,
}

#[derive(Default)]
struct OwnedPaths {
    root: Option<(PathBuf, FileId)>,
    files: Vec<(PathBuf, FileId)>,
    dirs: Vec<(PathBuf, FileId)>,
}

#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq)]
struct FileId(u64, u64);

#[cfg(unix)]
fn file_id(meta: &fs::Metadata) -> FileId {
    FileId(meta.dev(), meta.ino())
}

fn path_id(path: &Path) -> io::Result<FileId> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            format!("symlink in restore destination: {}", path.display()),
        ));
    }
    Ok(file_id(&meta))
}

fn restore_io(path: &Path, action: &str, error: impl std::fmt::Display) -> CoreError {
    CoreError::Io(io::Error::other(format!(
        "{} {}: {}",
        action,
        path.display(),
        error
    )))
}

fn ensure_dir(path: &Path, owned: &mut OwnedPaths) -> Result<(), CoreError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(restore_io(path, "unsafe directory", "not a real directory")),
        Err(e) if e.kind() == ErrorKind::NotFound => {
            if let Some(parent) = path.parent().filter(|parent| *parent != path) {
                ensure_dir(parent, owned)?;
            }
            let parent = path
                .parent()
                .ok_or_else(|| restore_io(path, "invalid directory", "no parent"))?;
            let canonical = fs::canonicalize(parent)
                .map_err(|e| restore_io(parent, "failed to resolve parent", e))?
                .join(
                    path.file_name()
                        .ok_or_else(|| restore_io(path, "invalid directory", "no name"))?,
                );
            fs::create_dir(&canonical)
                .map_err(|e| restore_io(&canonical, "failed to create directory", e))?;
            owned.dirs.push((canonical.clone(), path_id(&canonical)?));
            Ok(())
        }
        Err(e) => Err(restore_io(path, "failed to inspect directory", e)),
    }
}

fn check_destination_clean(dest: &Path, owned: &mut OwnedPaths) -> Result<PathBuf, CoreError> {
    for parent in dest.ancestors().skip(1) {
        #[cfg(target_os = "macos")]
        if parent == Path::new("/var") || parent == Path::new("/tmp") {
            continue;
        }
        match fs::symlink_metadata(parent) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(restore_io(parent, "unsafe destination parent", "symlink"));
            }
            Err(e) if e.kind() != ErrorKind::NotFound => {
                return Err(restore_io(
                    parent,
                    "failed to inspect destination parent",
                    e,
                ));
            }
            _ => {}
        }
    }
    match fs::symlink_metadata(dest) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
            if fs::read_dir(dest)
                .map_err(|e| restore_io(dest, "failed to read destination", e))?
                .next()
                .transpose()
                .map_err(|e| restore_io(dest, "failed to read destination", e))?
                .is_some()
            {
                return Err(CoreError::Io(io::Error::new(
                    ErrorKind::AlreadyExists,
                    format!(
                        "destination directory exists and is not empty: {}",
                        dest.display()
                    ),
                )));
            }
        }
        Ok(_) => {
            return Err(restore_io(
                dest,
                "unsafe destination",
                "not a real directory",
            ));
        }
        Err(e) if e.kind() == ErrorKind::NotFound => ensure_dir(dest, owned)?,
        Err(e) => return Err(restore_io(dest, "failed to inspect destination", e)),
    }
    let root =
        fs::canonicalize(dest).map_err(|e| restore_io(dest, "failed to resolve destination", e))?;
    owned.root = Some((root.clone(), path_id(&root)?));
    Ok(root)
}

impl OwnedPaths {
    fn still_under_root(&self, path: &Path) -> bool {
        let Some((root, id)) = &self.root else {
            return false;
        };
        if !path.starts_with(root) || path_id(root).ok() != Some(*id) {
            return false;
        }
        self.dirs
            .iter()
            .all(|(dir, expected)| !path.starts_with(dir) || path_id(dir).ok() == Some(*expected))
    }

    fn cleanup(&self) {
        for (path, id) in self.files.iter().rev() {
            if self.still_under_root(path) && path_id(path).ok() == Some(*id) {
                let _ = fs::remove_file(path);
            }
        }
        for (path, id) in self.dirs.iter().rev() {
            let safe = if let Some((root, _)) = &self.root {
                if path.starts_with(root) {
                    self.still_under_root(path)
                } else {
                    root.starts_with(path)
                        && fs::canonicalize(path).ok().as_deref() == Some(path.as_path())
                }
            } else {
                false
            };
            if safe && path_id(path).ok() == Some(*id) {
                let _ = fs::remove_dir(path);
            }
        }
    }
}

pub fn run_restore(
    version_id: &str,
    dest: impl AsRef<Path>,
    server_url: &str,
    options: RestoreOptions,
) -> Result<(), CoreError> {
    let mut owned = OwnedPaths::default();
    let result = (|| {
        let absolute_dest = if dest.as_ref().is_absolute() {
            dest.as_ref().to_path_buf()
        } else {
            std::env::current_dir()?.join(dest.as_ref())
        };
        let dest = check_destination_clean(&absolute_dest, &mut owned)?;
        let server_url = server_url.trim_end_matches('/');
        let client = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .new_agent();

        let manifest_url = format!("{}/v1/versions/{}/manifest", server_url, version_id);
        let mut manifest_res = client.get(&manifest_url).call().map_err(|e| {
            CoreError::Io(io::Error::other(format!("failed to fetch manifest: {}", e)))
        })?;
        if manifest_res.status().as_u16() != 200 {
            return Err(CoreError::Io(io::Error::other(format!(
                "manifest not found (status {})",
                manifest_res.status()
            ))));
        }
        let manifest_bytes = manifest_res.body_mut().read_to_vec().map_err(|e| {
            CoreError::Io(io::Error::other(format!("failed to read manifest: {}", e)))
        })?;
        let manifest = Manifest::from_bytes(&manifest_bytes)?;
        execute_restore_plan(&manifest, &dest, server_url, &options, &client, &mut owned)?;
        println!("✔ Restore completed successfully into {}", dest.display());
        Ok(())
    })();
    if result.is_err() {
        owned.cleanup();
    }
    result.map_err(|e| {
        CoreError::Io(io::Error::other(format!(
            "restore version {} into {}: {}",
            version_id,
            dest.as_ref().display(),
            e
        )))
    })
}

fn execute_restore_plan(
    manifest: &Manifest,
    dest: &Path,
    server_url: &str,
    options: &RestoreOptions,
    client: &ureq::Agent,
    owned: &mut OwnedPaths,
) -> Result<(), CoreError> {
    let mut planned_chunks = Vec::new();
    let mut file_mtimes = Vec::new();
    let mut dir_mtimes = Vec::new();

    for entry in &manifest.entries {
        let path = entry.path();
        let target = resolve_under_root(dest, path)
            .map_err(|e| restore_io(&dest.join(path), "invalid restore path", e))?;
        if !owned.still_under_root(&target) {
            return Err(restore_io(
                &target,
                "unsafe restore path",
                "destination was replaced",
            ));
        }
        match entry {
            ManifestEntry::Dir { mtime, .. } => {
                ensure_dir(&target, owned)?;
                dir_mtimes.push((target, *mtime));
            }
            ManifestEntry::File {
                size,
                mtime,
                chunks,
                ..
            } => {
                if let Some(parent) = target.parent() {
                    ensure_dir(parent, owned)?;
                }
                resolve_under_root(dest, path)
                    .map_err(|e| restore_io(&target, "invalid restore path", e))?;
                if !owned.still_under_root(&target) {
                    return Err(restore_io(
                        &target,
                        "unsafe restore path",
                        "parent was replaced",
                    ));
                }
                let file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&target)
                    .map_err(|e| restore_io(&target, "failed to create file", e))?;
                owned.files.push((
                    target.clone(),
                    file_id(
                        &file
                            .metadata()
                            .map_err(|e| restore_io(&target, "failed to inspect file", e))?,
                    ),
                ));
                file.set_len(*size)
                    .map_err(|e| restore_io(&target, "failed to size file", e))?;
                let file = Arc::new(file);
                file_mtimes.push((target.clone(), Arc::clone(&file), *mtime));
                let mut offset = 0;
                for (chunk_id, len) in chunks {
                    planned_chunks.push(PlannedChunk {
                        chunk_id: chunk_id.clone(),
                        target_path: target.clone(),
                        file: Arc::clone(&file),
                        offset,
                        len: *len,
                    });
                    offset += *len;
                }
            }
        }
    }

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(options.jobs)
        .build()
        .map_err(|e| CoreError::Io(io::Error::other(e)))?;
    pool.install(|| {
        planned_chunks
            .par_iter()
            .try_for_each(|item| -> Result<(), CoreError> {
                let chunk_context =
                    |e: &str| restore_io(&item.target_path, &format!("chunk {}", item.chunk_id), e);
                let chunk_url = format!("{}/v1/chunks/{}", server_url, item.chunk_id);
                let mut chunk_res = client
                    .get(&chunk_url)
                    .call()
                    .map_err(|e| chunk_context(&format!("download failed: {}", e)))?;
                if chunk_res.status().as_u16() != 200 {
                    return Err(chunk_context(&format!(
                        "download status {}",
                        chunk_res.status()
                    )));
                }
                let data = chunk_res
                    .body_mut()
                    .read_to_vec()
                    .map_err(|e| chunk_context(&format!("read failed: {}", e)))?;
                if data.len() as u64 != item.len {
                    return Err(chunk_context(&format!(
                        "size mismatch: expected {}, got {}",
                        item.len,
                        data.len()
                    )));
                }
                let hash = sha256_hex(&data);
                if hash != item.chunk_id {
                    return Err(chunk_context(&format!("hash mismatch: got {}", hash)));
                }
                if !owned.still_under_root(&item.target_path)
                    || path_id(&item.target_path).ok()
                        != Some(file_id(
                            &item
                                .file
                                .metadata()
                                .map_err(|e| chunk_context(&e.to_string()))?,
                        ))
                {
                    return Err(chunk_context("file was replaced during restore"));
                }
                #[cfg(unix)]
                item.file
                    .write_all_at(&data, item.offset)
                    .map_err(|e| chunk_context(&format!("write failed: {}", e)))?;
                Ok(())
            })
    })?;

    for (path, file, (secs, nsecs)) in file_mtimes {
        if !owned.still_under_root(&path)
            || path_id(&path).ok()
                != Some(file_id(
                    &file
                        .metadata()
                        .map_err(|e| restore_io(&path, "failed to inspect file", e))?,
                ))
        {
            return Err(restore_io(
                &path,
                "failed to set file mtime",
                "file was replaced",
            ));
        }
        if nsecs >= 1_000_000_000 {
            return Err(restore_io(
                &path,
                "failed to set file mtime",
                "invalid nanoseconds",
            ));
        }
        let ft = filetime::FileTime::from_unix_time(secs, nsecs);
        filetime::set_file_handle_times(&file, Some(ft), Some(ft))
            .map_err(|e| restore_io(&path, "failed to set file mtime", e))?;
    }

    dir_mtimes.sort_by(|a, b| b.0.cmp(&a.0));
    for (path, (secs, nsecs)) in dir_mtimes {
        if !owned.still_under_root(&path) {
            return Err(restore_io(
                &path,
                "failed to set directory mtime",
                "directory was replaced",
            ));
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(target_os = "macos")]
        options.custom_flags(0x100);
        #[cfg(target_os = "linux")]
        options.custom_flags(0x20000);
        let dir = options
            .open(&path)
            .map_err(|e| restore_io(&path, "failed to open directory for mtime", e))?;
        if owned
            .dirs
            .iter()
            .find(|(p, _)| p == &path)
            .map(|(_, id)| *id)
            != Some(file_id(&dir.metadata().map_err(|e| {
                restore_io(&path, "failed to inspect directory", e)
            })?))
        {
            return Err(restore_io(
                &path,
                "failed to set directory mtime",
                "directory was replaced",
            ));
        }
        if nsecs >= 1_000_000_000 {
            return Err(restore_io(
                &path,
                "failed to set directory mtime",
                "invalid nanoseconds",
            ));
        }
        let ft = filetime::FileTime::from_unix_time(secs, nsecs);
        filetime::set_file_handle_times(&dir, Some(ft), Some(ft))
            .map_err(|e| restore_io(&path, "failed to set directory mtime", e))?;
    }
    Ok(())
}
