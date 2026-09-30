use crate::core::errors::CoreError;
use crate::core::hash::{sha256_hex, sha256_reader};
use crate::core::manifest::valid_chunk_id;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Store {
    vault_dir: PathBuf,
}

impl Store {
    pub fn new(vault_dir: impl AsRef<Path>) -> Result<Self, CoreError> {
        let vault_dir = vault_dir.as_ref().to_path_buf();
        let chunks_dir = vault_dir.join("chunks");
        let tmp_dir = vault_dir.join("tmp");
        let quarantine_dir = vault_dir.join("quarantine");

        fs::create_dir_all(&chunks_dir)?;
        fs::create_dir_all(&tmp_dir)?;
        fs::create_dir_all(&quarantine_dir)?;

        Self::clean_tmp_directory(&tmp_dir)?;
        fs::write(vault_dir.join(".brokenvault_vault"), b"brokenvault")?;

        Ok(Self { vault_dir })
    }

    pub fn open_read_only(vault_dir: impl AsRef<Path>) -> Result<Self, CoreError> {
        let vault_dir = vault_dir.as_ref();
        if !vault_dir.is_dir() {
            return Err(CoreError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("vault directory not found: {}", vault_dir.display()),
            )));
        }
        Ok(Self {
            vault_dir: vault_dir.to_path_buf(),
        })
    }

    fn checked_chunk_path(&self, chunk_id: &str) -> Result<PathBuf, CoreError> {
        if !valid_chunk_id(chunk_id) {
            return Err(CoreError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid chunk id: expected 64 lowercase hex characters",
            )));
        }
        Ok(self.chunk_path(chunk_id))
    }

    fn clean_tmp_directory(tmp_dir: &Path) -> Result<(), CoreError> {
        if tmp_dir.exists() {
            for entry in fs::read_dir(tmp_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.is_file() {
                    let _ = fs::remove_file(path);
                } else if path.is_dir() {
                    let _ = fs::remove_dir_all(path);
                }
            }
        }
        Ok(())
    }

    pub fn chunk_path(&self, chunk_id: &str) -> PathBuf {
        if !valid_chunk_id(chunk_id) {
            return self
                .vault_dir
                .join("chunks")
                .join("invalid")
                .join(sha256_hex(chunk_id.as_bytes()));
        }
        self.vault_dir
            .join("chunks")
            .join(&chunk_id[..2])
            .join(chunk_id)
    }

    pub fn has_chunk(&self, chunk_id: &str, expected_len: u64) -> bool {
        let Ok(path) = self.checked_chunk_path(chunk_id) else {
            return false;
        };
        if let Ok(meta) = fs::metadata(&path) {
            meta.is_file() && meta.len() == expected_len
        } else {
            false
        }
    }

    pub fn read_chunk(&self, chunk_id: &str) -> Result<Vec<u8>, CoreError> {
        let path = self.checked_chunk_path(chunk_id)?;
        let data = fs::read(&path)?;
        Ok(data)
    }

    pub fn write_chunk_tmp(&self, chunk_id: &str, data: &[u8]) -> Result<PathBuf, CoreError> {
        self.checked_chunk_path(chunk_id)?;
        let rand_suffix: u64 = rand::random();
        let tmp_name = format!("{}_{:016x}.tmp", chunk_id, rand_suffix);
        let tmp_path = self.vault_dir.join("tmp").join(tmp_name);

        let mut file = File::create(&tmp_path)?;
        file.write_all(data)?;
        file.flush()?;
        Ok(tmp_path)
    }

    pub fn promote_tmp_chunk(&self, tmp_path: &Path, chunk_id: &str) -> Result<PathBuf, CoreError> {
        let final_path = self.checked_chunk_path(chunk_id)?;
        if tmp_path.parent() != Some(self.vault_dir.join("tmp").as_path()) {
            return Err(CoreError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "temporary chunk path is outside the store tmp directory",
            )));
        }
        if let Some(parent) = final_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(tmp_path, &final_path)?;
        Ok(final_path)
    }

    pub fn quarantine_chunk(&self, chunk_id: &str) -> Result<Option<PathBuf>, CoreError> {
        let path = self.checked_chunk_path(chunk_id)?;
        if path.exists() {
            let rand_suffix: u64 = rand::random();
            let quarantine_name = format!("{}_{:016x}.corrupt", chunk_id, rand_suffix);
            let quarantine_path = self.vault_dir.join("quarantine").join(quarantine_name);
            fs::rename(&path, &quarantine_path)?;
            Ok(Some(quarantine_path))
        } else {
            Ok(None)
        }
    }

    pub fn verify_chunk_on_disk(
        &self,
        chunk_id: &str,
        expected_len: u64,
    ) -> Result<(), ChunkVerificationError> {
        let path = self
            .checked_chunk_path(chunk_id)
            .map_err(|_| ChunkVerificationError::Missing)?;
        let meta = fs::metadata(&path).map_err(|_| ChunkVerificationError::Missing)?;
        if !meta.is_file() {
            return Err(ChunkVerificationError::Missing);
        }
        if meta.len() != expected_len {
            return Err(ChunkVerificationError::SizeMismatch {
                expected: expected_len,
                actual: meta.len(),
            });
        }
        let file = File::open(&path).map_err(|_| ChunkVerificationError::Missing)?;
        let computed = sha256_reader(file).map_err(|_| ChunkVerificationError::Missing)?;
        if computed != chunk_id {
            return Err(ChunkVerificationError::HashMismatch {
                expected: chunk_id.to_string(),
                actual: computed,
            });
        }
        Ok(())
    }

    pub fn sync_chunks(&self, chunk_ids: &[String]) -> Result<(), CoreError> {
        for chunk_id in chunk_ids {
            let path = self.checked_chunk_path(chunk_id)?;
            if path.exists() {
                let file = File::open(&path)?;
                file.sync_all()?;
                if let Some(parent) = path.parent() {
                    let dir = File::open(parent)?;
                    dir.sync_all()?;
                }
            }
        }
        Ok(())
    }

    pub fn vault_dir(&self) -> &Path {
        &self.vault_dir
    }

    pub fn storage_stats(&self) -> Result<(usize, u64), CoreError> {
        let chunks_dir = self.vault_dir.join("chunks");
        if !chunks_dir.exists() {
            return Ok((0, 0));
        }
        let mut count = 0usize;
        let mut bytes = 0u64;
        for prefix_entry in fs::read_dir(chunks_dir)? {
            let prefix_entry = prefix_entry?;
            if prefix_entry.path().is_dir() {
                for chunk_entry in fs::read_dir(prefix_entry.path())? {
                    let chunk_entry = chunk_entry?;
                    let meta = chunk_entry.metadata()?;
                    if meta.is_file() {
                        count = count.saturating_add(1);
                        bytes = bytes.saturating_add(meta.len());
                    }
                }
            }
        }
        Ok((count, bytes))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ChunkVerificationError {
    Missing,
    SizeMismatch { expected: u64, actual: u64 },
    HashMismatch { expected: String, actual: String },
}
