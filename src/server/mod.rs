pub mod api;
pub mod commit;
pub mod db;
pub mod debug;
pub mod store;
pub mod verify;

use crate::core::errors::CoreError;
use crate::core::proto::VaultStats;
use crate::server::db::Database;
use crate::server::store::Store;

pub fn compute_vault_stats(store: &Store, db: &Database) -> Result<VaultStats, CoreError> {
    let (unique_chunks, total_physical_chunk_bytes) = store.storage_stats()?;
    let vault_id = db.vault_id()?;
    let (completed_versions, open_uploads, total_logical_bytes) = db.stats()?;
    let deduplication_ratio = if total_physical_chunk_bytes > 0 {
        total_logical_bytes as f64 / total_physical_chunk_bytes as f64
    } else if total_logical_bytes > 0 {
        f64::INFINITY
    } else {
        1.0
    };
    let space_saved_bytes = total_logical_bytes.saturating_sub(total_physical_chunk_bytes);
    let space_saved_percent = if total_logical_bytes > 0 {
        (space_saved_bytes as f64 / total_logical_bytes as f64) * 100.0
    } else {
        0.0
    };
    Ok(VaultStats {
        vault_id,
        completed_versions,
        open_uploads,
        total_logical_bytes,
        total_physical_chunk_bytes,
        unique_chunks,
        deduplication_ratio,
        space_saved_bytes,
        space_saved_percent,
    })
}
