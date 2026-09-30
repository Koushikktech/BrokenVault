use crate::core::errors::CoreError;
use crate::core::hash::{sha256_hex, sha256_reader};
use crate::core::manifest::{Manifest, ManifestEntry};
use crate::core::proto::{ChunkReference, DamageType, DamagedChunkReport, VerifyReport};
use crate::server::db::Database;
use crate::server::store::Store;
use rayon::prelude::*;
use std::collections::{BTreeSet, HashMap};
use std::fs::File;

pub fn execute_verification(store: &Store, db: &Database) -> Result<VerifyReport, CoreError> {
    let completed_manifests = db.list_completed_manifests()?;
    let mut chunk_refs: HashMap<String, (u64, Vec<ChunkReference>)> = HashMap::new();
    let mut version_ids = BTreeSet::new();

    for (version_id, manifest_bytes) in &completed_manifests {
        version_ids.insert(version_id.clone());
        let computed_manifest_hash = sha256_hex(manifest_bytes);

        let manifest = Manifest::from_bytes(manifest_bytes)?;
        let expected_manifest_hash = manifest.manifest_id()?;
        if computed_manifest_hash != expected_manifest_hash {
            return Err(CoreError::Io(std::io::Error::other(format!(
                "manifest hash mismatch for version {}: database corrupted",
                version_id
            ))));
        }

        for entry in &manifest.entries {
            if let ManifestEntry::File { path, chunks, .. } = entry {
                let mut offset = 0;
                for (chunk_idx, (chunk_id, len)) in chunks.iter().enumerate() {
                    let end_byte = offset + len.saturating_sub(1);
                    let reference = ChunkReference {
                        version_id: version_id.clone(),
                        path: path.clone(),
                        chunk_index: chunk_idx,
                        start_byte: offset,
                        end_byte,
                    };
                    let entry = chunk_refs
                        .entry(chunk_id.clone())
                        .or_insert_with(|| (*len, Vec::new()));
                    entry.1.push(reference);
                    offset += *len;
                }
            }
        }
    }

    let chunk_refs_vec: Vec<(String, (u64, Vec<ChunkReference>))> =
        chunk_refs.into_iter().collect();

    let scan_results: Vec<Option<DamagedChunkReport>> = chunk_refs_vec
        .par_iter()
        .map(|(chunk_id, (expected_len, refs))| {
            let path = store.chunk_path(chunk_id);
            if !path.exists() {
                return Some(DamagedChunkReport {
                    chunk_id: chunk_id.clone(),
                    damage_type: DamageType::Missing,
                    expected_len: *expected_len,
                    actual_len: None,
                    affected: refs.clone(),
                });
            }

            let meta = match std::fs::metadata(&path) {
                Ok(m) => m,
                Err(_) => {
                    return Some(DamagedChunkReport {
                        chunk_id: chunk_id.clone(),
                        damage_type: DamageType::Missing,
                        expected_len: *expected_len,
                        actual_len: None,
                        affected: refs.clone(),
                    });
                }
            };

            if meta.len() != *expected_len {
                return Some(DamagedChunkReport {
                    chunk_id: chunk_id.clone(),
                    damage_type: DamageType::SizeMismatch,
                    expected_len: *expected_len,
                    actual_len: Some(meta.len()),
                    affected: refs.clone(),
                });
            }

            let file = match File::open(&path) {
                Ok(f) => f,
                Err(_) => {
                    return Some(DamagedChunkReport {
                        chunk_id: chunk_id.clone(),
                        damage_type: DamageType::Missing,
                        expected_len: *expected_len,
                        actual_len: Some(meta.len()),
                        affected: refs.clone(),
                    });
                }
            };

            let computed = match sha256_reader(file) {
                Ok(h) => h,
                Err(_) => {
                    return Some(DamagedChunkReport {
                        chunk_id: chunk_id.clone(),
                        damage_type: DamageType::Missing,
                        expected_len: *expected_len,
                        actual_len: Some(meta.len()),
                        affected: refs.clone(),
                    });
                }
            };

            if computed != *chunk_id {
                return Some(DamagedChunkReport {
                    chunk_id: chunk_id.clone(),
                    damage_type: DamageType::HashMismatch,
                    expected_len: *expected_len,
                    actual_len: Some(meta.len()),
                    affected: refs.clone(),
                });
            }

            None
        })
        .collect();

    let mut damaged_chunks = Vec::new();
    let mut affected_versions = BTreeSet::new();

    for report in scan_results.into_iter().flatten() {
        for aff in &report.affected {
            affected_versions.insert(aff.version_id.clone());
        }
        damaged_chunks.push(report);
    }

    let healthy_versions: Vec<String> = version_ids
        .difference(&affected_versions)
        .cloned()
        .collect();

    let mut info_messages = Vec::new();
    let open_uploads = db.list_open_uploads()?;
    if !open_uploads.is_empty() {
        info_messages.push(format!(
            "Note: {} open upload(s) in progress",
            open_uploads.len()
        ));
    }

    let healthy = damaged_chunks.is_empty();

    Ok(VerifyReport {
        healthy,
        damaged_chunks,
        healthy_versions,
        info_messages,
    })
}

pub fn print_verify_report(report: &VerifyReport) {
    if report.healthy {
        println!("✔ INTEGRITY OK — all referenced chunks verified byte-for-byte");
        println!(
            "  Healthy completed versions: {}",
            if report.healthy_versions.is_empty() {
                "none".to_string()
            } else {
                report.healthy_versions.join(", ")
            }
        );
        for info in &report.info_messages {
            println!("  {}", info);
        }
        println!("  No files were modified.");
    } else {
        let mut affected_set = BTreeSet::new();
        for d in &report.damaged_chunks {
            for aff in &d.affected {
                affected_set.insert(aff.version_id.clone());
            }
        }

        println!(
            "✖ DAMAGE FOUND — {} chunk(s), {} completed version(s) affected           (exit code 1)",
            report.damaged_chunks.len(),
            affected_set.len()
        );

        for d in &report.damaged_chunks {
            let short_id = if d.chunk_id.len() >= 10 {
                format!(
                    "{}…{}",
                    &d.chunk_id[..4],
                    &d.chunk_id[d.chunk_id.len() - 4..]
                )
            } else {
                d.chunk_id.clone()
            };

            let damage_label = match d.damage_type {
                DamageType::Missing => "MISSING".to_string(),
                DamageType::SizeMismatch => format!(
                    "SIZE_MISMATCH (expected {} B, actual {} B)",
                    d.expected_len,
                    d.actual_len.unwrap_or(0)
                ),
                DamageType::HashMismatch => {
                    format!("HASH_MISMATCH (expected {} B)", d.expected_len)
                }
            };

            println!("  chunk {}  {}", short_id, damage_label);
            for (idx, aff) in d.affected.iter().enumerate() {
                let is_last = idx == d.affected.len() - 1;
                let branch = if is_last { "└─" } else { "├─" };
                println!(
                    "    {} {}  {}   chunk #{}  bytes {}–{}",
                    branch, aff.version_id, aff.path, aff.chunk_index, aff.start_byte, aff.end_byte
                );
            }
        }

        println!(
            "  Healthy versions: {}.   No files were modified.",
            if report.healthy_versions.is_empty() {
                "none".to_string()
            } else {
                report.healthy_versions.join(", ")
            }
        );
    }
}
