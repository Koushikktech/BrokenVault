use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthResponse {
    pub vault_id: String,
    pub protocol_version: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadInitResponse {
    pub upload_id: String,
    pub resumed: bool,
    pub total_bytes: u64,
    pub chunks_total: usize,
    pub missing: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadStatusResponse {
    pub upload_id: String,
    pub state: String,
    pub total_bytes: u64,
    pub chunks_total: usize,
    pub missing: Vec<String>,
    pub version_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenUploadSummary {
    pub upload_id: String,
    pub total_bytes: u64,
    pub created_at: i64,
    pub state: String,
    pub chunks_total: usize,
    pub chunks_present: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitResponse {
    pub version: String,
    pub committed_at: i64,
    pub total_bytes: u64,
    pub uploaded_bytes: u64,
    pub reused_bytes: u64,
    pub files: usize,
    pub dirs: usize,
    pub chunks: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitConflictResponse {
    pub missing_or_corrupt: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionSummary {
    pub id: String,
    pub upload_id: String,
    pub manifest_id: String,
    pub committed_at: i64,
    pub total_bytes: u64,
    pub uploaded_bytes: u64,
    pub reused_bytes: u64,
    pub files: usize,
    pub dirs: usize,
    pub chunks: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DamageType {
    Missing,
    SizeMismatch,
    HashMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkReference {
    pub version_id: String,
    pub path: String,
    pub chunk_index: usize,
    pub start_byte: u64,
    pub end_byte: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DamagedChunkReport {
    pub chunk_id: String,
    pub damage_type: DamageType,
    pub expected_len: u64,
    pub actual_len: Option<u64>,
    pub affected: Vec<ChunkReference>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyReport {
    pub healthy: bool,
    pub damaged_chunks: Vec<DamagedChunkReport>,
    pub healthy_versions: Vec<String>,
    pub info_messages: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl ApiError {
    pub fn new(code: impl Into<String>, message: impl Into<String>, hint: Option<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            hint,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VaultStats {
    pub vault_id: String,
    pub completed_versions: usize,
    pub open_uploads: usize,
    pub total_logical_bytes: u64,
    pub total_physical_chunk_bytes: u64,
    pub unique_chunks: usize,
    pub deduplication_ratio: f64,
    pub space_saved_bytes: u64,
    pub space_saved_percent: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffChangeType {
    Added,
    Removed,
    Modified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDiff {
    pub path: String,
    pub change: DiffChangeType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_size: Option<u64>,
    pub new_chunks: usize,
    pub reused_chunks: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionDiffReport {
    pub from_version: String,
    pub to_version: String,
    pub files: Vec<FileDiff>,
    pub files_added: usize,
    pub files_removed: usize,
    pub files_modified: usize,
    pub total_new_bytes: u64,
    pub total_reused_bytes: u64,
}
