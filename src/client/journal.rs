use crate::core::errors::CoreError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    pub upload_id: String,
    pub server_url: String,
    pub manifest_id: String,
    pub created_at: i64,
    pub committed: bool,
    pub version_id: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Journal {
    pub entries: HashMap<String, JournalEntry>,
}

impl Journal {
    pub fn journal_path() -> PathBuf {
        if let Ok(dir) = std::env::var("BV_STATE_DIR") {
            PathBuf::from(dir).join("journal.json")
        } else if let Ok(home) = std::env::var("HOME") {
            PathBuf::from(home)
                .join(".local")
                .join("state")
                .join("brokenvault")
                .join("journal.json")
        } else {
            PathBuf::from("journal.json")
        }
    }

    pub fn load_from_disk() -> Self {
        let path = Self::journal_path();
        if path.exists() {
            if let Ok(bytes) = fs::read(&path) {
                if let Ok(journal) = serde_json::from_slice::<Self>(&bytes) {
                    return journal;
                }
            }
        }
        Self::default()
    }

    pub fn save_to_disk(&self) -> Result<(), CoreError> {
        let path = Self::journal_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self)?;
        let mut file = File::create(&path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        Ok(())
    }

    pub fn get(&self, key: &str) -> Option<&JournalEntry> {
        self.entries.get(key)
    }

    pub fn insert(&mut self, key: String, entry: JournalEntry) {
        self.entries.insert(key, entry);
    }

    pub fn remove(&mut self, key: &str) -> Option<JournalEntry> {
        self.entries.remove(key)
    }
}

pub fn make_journal_key(server_url: &str, manifest_id: &str) -> String {
    format!("{}#{}", server_url.trim_end_matches('/'), manifest_id)
}
