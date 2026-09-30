use crate::core::chunker::{CHUNK_AVG_SIZE, CHUNK_MAX_SIZE, CHUNK_MIN_SIZE, CHUNKER_ALGO};
use crate::core::errors::{ManifestError, PathError};
use crate::core::hash::sha256_hex;
use crate::core::pathsafe::validate_relative_path;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

pub fn valid_chunk_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub const MANIFEST_FORMAT: &str = "brokenvault-manifest/1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkerConfig {
    pub algo: String,
    pub min: u32,
    pub avg: u32,
    pub max: u32,
}

impl Default for ChunkerConfig {
    fn default() -> Self {
        Self {
            algo: CHUNKER_ALGO.to_string(),
            min: CHUNK_MIN_SIZE,
            avg: CHUNK_AVG_SIZE,
            max: CHUNK_MAX_SIZE,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ManifestEntry {
    #[serde(rename = "dir")]
    Dir { path: String, mtime: (i64, u32) },
    #[serde(rename = "file")]
    File {
        path: String,
        size: u64,
        mtime: (i64, u32),
        chunks: Vec<(String, u64)>,
    },
}

impl ManifestEntry {
    pub fn path(&self) -> &str {
        match self {
            ManifestEntry::Dir { path, .. } => path,
            ManifestEntry::File { path, .. } => path,
        }
    }

    pub fn mtime(&self) -> (i64, u32) {
        match self {
            ManifestEntry::Dir { mtime, .. } => *mtime,
            ManifestEntry::File { mtime, .. } => *mtime,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: String,
    pub chunker: ChunkerConfig,
    pub entries: Vec<ManifestEntry>,
}

impl Manifest {
    pub fn new(mut entries: Vec<ManifestEntry>) -> Result<Self, ManifestError> {
        entries.sort_by(|a, b| a.path().cmp(b.path()));
        let manifest = Self {
            format: MANIFEST_FORMAT.to_string(),
            chunker: ChunkerConfig::default(),
            entries,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.format != MANIFEST_FORMAT {
            return Err(ManifestError::InvalidFormat(self.format.clone()));
        }
        if self.chunker != ChunkerConfig::default() {
            return Err(ManifestError::InvalidChunker(format!(
                "unsupported chunker configuration: {:?}",
                self.chunker
            )));
        }

        let mut seen = HashSet::new();
        let mut chunk_lengths = HashMap::new();
        let mut total_bytes = 0u64;
        let mut dir_prefixes = HashSet::new();
        let mut previous_path: Option<&str> = None;

        for entry in &self.entries {
            let path = entry.path();
            validate_relative_path(path).map_err(|e| match e {
                PathError::DuplicatePath(p) => ManifestError::DuplicateEntry(p),
                other => ManifestError::InvalidFormat(other.to_string()),
            })?;

            if let Some(prev) = previous_path {
                if prev >= path {
                    return Err(ManifestError::InvalidFormat(format!(
                        "entries not strictly sorted: {} vs {}",
                        prev, path
                    )));
                }
            }
            previous_path = Some(path);

            if !seen.insert(path) {
                return Err(ManifestError::DuplicateEntry(path.to_string()));
            }

            let parts: Vec<&str> = path.split('/').collect();
            let mut current = String::new();
            let parent_count = parts.len().saturating_sub(1);
            for part in parts.iter().take(parent_count) {
                if !current.is_empty() {
                    current.push('/');
                }
                current.push_str(part);
                dir_prefixes.insert(current.clone());
            }

            if let ManifestEntry::File {
                path, size, chunks, ..
            } = entry
            {
                let chunk_sum = chunks.iter().try_fold(0u64, |sum, (_, len)| {
                    sum.checked_add(*len).ok_or_else(|| {
                        ManifestError::InvalidFormat(format!(
                            "chunk length sum overflows for {path}"
                        ))
                    })
                })?;
                if chunk_sum != *size {
                    return Err(ManifestError::ChunkSumMismatch {
                        path: path.clone(),
                        expected: *size,
                        actual: chunk_sum,
                    });
                }
                total_bytes = total_bytes.checked_add(*size).ok_or_else(|| {
                    ManifestError::InvalidFormat("manifest total size overflows".to_string())
                })?;
                for (id, len) in chunks {
                    if !valid_chunk_id(id) {
                        return Err(ManifestError::InvalidFormat(format!(
                            "invalid chunk id: {id}"
                        )));
                    }
                    if let Some(previous) = chunk_lengths.insert(id.as_str(), *len) {
                        if previous != *len {
                            return Err(ManifestError::InvalidFormat(format!(
                                "inconsistent lengths for chunk {id}: {previous} vs {len}"
                            )));
                        }
                    }
                }
            }
        }

        for entry in &self.entries {
            if let ManifestEntry::File { path, .. } = entry {
                if dir_prefixes.contains(path.as_str()) {
                    return Err(ManifestError::InvalidFormat(format!(
                        "file path conflicts with directory prefix: {}",
                        path
                    )));
                }
            }
        }

        Ok(())
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ManifestError> {
        self.validate()?;
        serde_json::to_vec(self).map_err(ManifestError::from)
    }

    pub fn manifest_id(&self) -> Result<String, ManifestError> {
        let bytes = self.canonical_bytes()?;
        Ok(sha256_hex(&bytes))
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ManifestError> {
        let manifest: Manifest = serde_json::from_slice(bytes)?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn unique_chunk_ids(&self) -> Vec<String> {
        let mut ids = HashSet::new();
        let mut ordered = Vec::new();
        for entry in &self.entries {
            if let ManifestEntry::File { chunks, .. } = entry {
                for (id, _) in chunks {
                    if ids.insert(id.clone()) {
                        ordered.push(id.clone());
                    }
                }
            }
        }
        ordered
    }

    pub fn total_bytes(&self) -> u64 {
        self.entries
            .iter()
            .map(|entry| match entry {
                ManifestEntry::File { size, .. } => *size,
                ManifestEntry::Dir { .. } => 0,
            })
            .sum()
    }

    pub fn total_files(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| matches!(entry, ManifestEntry::File { .. }))
            .count()
    }

    pub fn total_dirs(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| matches!(entry, ManifestEntry::Dir { .. }))
            .count()
    }

    pub fn total_chunks(&self) -> usize {
        self.entries
            .iter()
            .map(|entry| match entry {
                ManifestEntry::File { chunks, .. } => chunks.len(),
                ManifestEntry::Dir { .. } => 0,
            })
            .sum()
    }
}
