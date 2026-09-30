use crate::core::chunker::chunk_reader;
use crate::core::errors::CoreError;
use crate::core::manifest::{Manifest, ManifestEntry};
use crate::core::pathsafe::validate_relative_path;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub struct LocalChunkDetail {
    pub chunk_id: String,
    pub source_path: PathBuf,
    pub offset: u64,
    pub len: u64,
}

pub fn scan_directory(
    root: impl AsRef<Path>,
) -> Result<(Manifest, Vec<LocalChunkDetail>), CoreError> {
    let root = root.as_ref();
    if !root.exists() {
        return Err(CoreError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("source path does not exist: {}", root.display()),
        )));
    }

    let mut entries = Vec::new();
    let mut chunk_details = Vec::new();

    for walk_res in WalkDir::new(root).sort_by_file_name() {
        let entry = walk_res.map_err(std::io::Error::other)?;
        let path = entry.path();

        if path == root {
            continue;
        }

        let file_type = entry.file_type();
        if file_type.is_symlink() {
            eprintln!("Warning: skipping symlink {}", path.display());
            continue;
        }

        let rel_path = path
            .strip_prefix(root)
            .map_err(std::io::Error::other)?
            .to_str()
            .ok_or_else(|| {
                CoreError::Io(std::io::Error::other(format!(
                    "non-utf8 path encountered: {}",
                    path.display()
                )))
            })?
            .replace('\\', "/");

        validate_relative_path(&rel_path)?;

        let metadata = entry.metadata().map_err(std::io::Error::other)?;
        let mtime_ft = filetime::FileTime::from_last_modification_time(&metadata);
        let mtime = (mtime_ft.unix_seconds(), mtime_ft.nanoseconds());

        if file_type.is_dir() {
            entries.push(ManifestEntry::Dir {
                path: rel_path,
                mtime,
            });
        } else if file_type.is_file() {
            let size = metadata.len();
            if size == 0 {
                entries.push(ManifestEntry::File {
                    path: rel_path,
                    size: 0,
                    mtime,
                    chunks: Vec::new(),
                });
            } else {
                let file = File::open(path)?;
                let reader = BufReader::new(file);
                let chunks = chunk_reader(reader)?;

                let mut manifest_chunks = Vec::with_capacity(chunks.len());
                for chunk in chunks {
                    manifest_chunks.push((chunk.hash.clone(), chunk.len));
                    chunk_details.push(LocalChunkDetail {
                        chunk_id: chunk.hash,
                        source_path: path.to_path_buf(),
                        offset: chunk.offset,
                        len: chunk.len,
                    });
                }

                entries.push(ManifestEntry::File {
                    path: rel_path,
                    size,
                    mtime,
                    chunks: manifest_chunks,
                });
            }
        } else {
            eprintln!("Warning: skipping non-regular file {}", path.display());
        }
    }

    let manifest = Manifest::new(entries)?;
    Ok((manifest, chunk_details))
}
