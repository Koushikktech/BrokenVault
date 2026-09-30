use crate::core::errors::CoreError;
use crate::server::store::Store;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DamageMode {
    Flip,
    Truncate,
    Delete,
}

impl std::str::FromStr for DamageMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "flip" => Ok(DamageMode::Flip),
            "truncate" => Ok(DamageMode::Truncate),
            "delete" => Ok(DamageMode::Delete),
            other => Err(format!("unknown damage mode: {}", other)),
        }
    }
}

pub fn apply_damage(
    store: &Store,
    mode: DamageMode,
    target_chunk_id: Option<&str>,
) -> Result<String, CoreError> {
    let chunk_id = match target_chunk_id {
        Some(id) => id.to_string(),
        None => find_first_chunk(store.vault_dir())?.ok_or_else(|| {
            CoreError::Io(std::io::Error::other(
                "no chunks available to damage in vault",
            ))
        })?,
    };

    let path = store.chunk_path(&chunk_id);
    if !path.exists() {
        return Err(CoreError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("chunk {} not found on disk", chunk_id),
        )));
    }

    match mode {
        DamageMode::Delete => {
            fs::remove_file(&path)?;
        }
        DamageMode::Truncate => {
            let meta = fs::metadata(&path)?;
            let new_len = meta.len().saturating_sub(1);
            let file = OpenOptions::new().write(true).open(&path)?;
            file.set_len(new_len)?;
            file.sync_all()?;
        }
        DamageMode::Flip => {
            let mut data = fs::read(&path)?;
            if !data.is_empty() {
                data[0] ^= 0xFF;
                let mut file = OpenOptions::new().write(true).truncate(true).open(&path)?;
                file.write_all(&data)?;
                file.sync_all()?;
            }
        }
    }

    Ok(chunk_id)
}

fn find_first_chunk(vault_dir: &Path) -> Result<Option<String>, CoreError> {
    let chunks_dir = vault_dir.join("chunks");
    if !chunks_dir.exists() {
        return Ok(None);
    }
    for prefix_entry in fs::read_dir(chunks_dir)? {
        let prefix_entry = prefix_entry?;
        if prefix_entry.path().is_dir() {
            for chunk_entry in fs::read_dir(prefix_entry.path())? {
                let chunk_entry = chunk_entry?;
                if chunk_entry.path().is_file() {
                    if let Some(name) = chunk_entry.file_name().to_str() {
                        return Ok(Some(name.to_string()));
                    }
                }
            }
        }
    }
    Ok(None)
}
