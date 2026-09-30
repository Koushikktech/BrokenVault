use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("path error: {0}")]
    Path(#[from] PathError),

    #[error("chunk error: {0}")]
    Chunk(#[from] ChunkError),

    #[error("manifest error: {0}")]
    Manifest(#[from] ManifestError),

    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("operation interrupted")]
    Interrupted,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PathError {
    #[error("path cannot be empty")]
    EmptyPath,

    #[error("absolute paths are rejected: {0}")]
    AbsolutePath(String),

    #[error("path traversal component rejected: {0}")]
    TraversalComponent(String),

    #[error("windows drive prefix rejected: {0}")]
    WindowsPrefix(String),

    #[error("backslash separator rejected: {0}")]
    BackslashSeparator(String),

    #[error("invalid control or null character in path: {0}")]
    InvalidCharacter(String),

    #[error("duplicate path entry in manifest: {0}")]
    DuplicatePath(String),

    #[error("file path conflicts with directory prefix: {0}")]
    FileDirectoryConflict(String),

    #[error("target path escapes destination directory: {0}")]
    DestinationEscape(String),
}

#[derive(Debug, Error)]
pub enum ChunkError {
    #[error("io error during chunking: {0}")]
    Io(#[from] std::io::Error),

    #[error("fastcdc error: {0}")]
    Cdc(#[from] fastcdc::v2020::Error),

    #[error("chunk size mismatch: expected {expected}, got {actual}")]
    SizeMismatch { expected: u64, actual: u64 },

    #[error("chunk hash mismatch: expected {expected}, got {actual}")]
    HashMismatch { expected: String, actual: String },
}

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("invalid manifest format: {0}")]
    InvalidFormat(String),

    #[error("invalid chunker configuration: {0}")]
    InvalidChunker(String),

    #[error(
        "file size does not match sum of chunk lengths for {path}: expected {expected}, actual {actual}"
    )]
    ChunkSumMismatch {
        path: String,
        expected: u64,
        actual: u64,
    },

    #[error("duplicate manifest entry: {0}")]
    DuplicateEntry(String),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}
