use brokenvault::core::chunker::chunk_slice;
use brokenvault::core::hash::sha256_hex;
use brokenvault::core::manifest::{Manifest, ManifestEntry};
use brokenvault::core::proto::DamageType;
use brokenvault::server::commit::execute_commit;
use brokenvault::server::db::Database;
use brokenvault::server::debug::{DamageMode, apply_damage};
use brokenvault::server::store::Store;
use brokenvault::server::verify::execute_verification;
use std::collections::BTreeSet;
use std::fs;
use tempfile::tempdir;
use walkdir::WalkDir;

#[test]
fn test_verification_healthy_damaged_and_readonly() {
    let tmp = tempdir().unwrap();
    let vault_dir = tmp.path().join("vault");
    let store = Store::new(&vault_dir).unwrap();
    let mut db = Database::open(vault_dir.join("meta.db")).unwrap();

    let data_shared = b"shared chunk data between v1 and v2";
    let chunks_shared = chunk_slice(data_shared).unwrap();
    let id_shared = chunks_shared[0].hash.clone();
    let len_shared = chunks_shared[0].len;

    let tmp_path = store.write_chunk_tmp(&id_shared, data_shared).unwrap();
    store.promote_tmp_chunk(&tmp_path, &id_shared).unwrap();

    let data_v1 = b"unique v1 chunk data";
    let chunks_v1 = chunk_slice(data_v1).unwrap();
    let id_v1 = chunks_v1[0].hash.clone();
    let len_v1 = chunks_v1[0].len;

    let tmp_path_v1 = store.write_chunk_tmp(&id_v1, data_v1).unwrap();
    store.promote_tmp_chunk(&tmp_path_v1, &id_v1).unwrap();

    let entries_v1 = vec![
        ManifestEntry::File {
            path: "shared.bin".to_string(),
            size: len_shared,
            mtime: (1000, 0),
            chunks: vec![(id_shared.clone(), len_shared)],
        },
        ManifestEntry::File {
            path: "only_v1.bin".to_string(),
            size: len_v1,
            mtime: (1001, 0),
            chunks: vec![(id_v1.clone(), len_v1)],
        },
    ];
    let m1 = Manifest::new(entries_v1).unwrap();
    let m1_bytes = m1.canonical_bytes().unwrap();
    let m1_id = m1.manifest_id().unwrap();

    db.store_manifest(&m1_id, &m1_bytes).unwrap();
    db.create_upload("up_v1", &m1_id, m1.total_bytes()).unwrap();
    db.record_accepted_chunk("up_v1", &id_shared, len_shared)
        .unwrap();
    db.record_accepted_chunk("up_v1", &id_v1, len_v1).unwrap();
    execute_commit(&store, &mut db, "up_v1").unwrap();

    let entries_v2 = vec![ManifestEntry::File {
        path: "sub/shared.bin".to_string(),
        size: len_shared,
        mtime: (2000, 0),
        chunks: vec![(id_shared.clone(), len_shared)],
    }];
    let m2 = Manifest::new(entries_v2).unwrap();
    let m2_bytes = m2.canonical_bytes().unwrap();
    let m2_id = m2.manifest_id().unwrap();

    db.store_manifest(&m2_id, &m2_bytes).unwrap();
    db.create_upload("up_v2", &m2_id, m2.total_bytes()).unwrap();
    execute_commit(&store, &mut db, "up_v2").unwrap();

    let healthy_report = execute_verification(&store, &db).unwrap();
    assert!(healthy_report.healthy);
    assert_eq!(healthy_report.healthy_versions, vec!["v1", "v2"]);
    assert!(healthy_report.damaged_chunks.is_empty());

    let vault_before_hash = compute_directory_tree_hash(&vault_dir);
    let _ = execute_verification(&store, &db).unwrap();
    let vault_after_hash = compute_directory_tree_hash(&vault_dir);
    assert_eq!(vault_before_hash, vault_after_hash);

    apply_damage(&store, DamageMode::Flip, Some(&id_shared)).unwrap();

    let damaged_report = execute_verification(&store, &db).unwrap();
    assert!(!damaged_report.healthy);
    assert_eq!(damaged_report.damaged_chunks.len(), 1);

    let damaged = &damaged_report.damaged_chunks[0];
    assert_eq!(damaged.chunk_id, id_shared);
    assert_eq!(damaged.damage_type, DamageType::HashMismatch);

    let affected_versions: BTreeSet<String> = damaged
        .affected
        .iter()
        .map(|aff| aff.version_id.clone())
        .collect();
    assert!(affected_versions.contains("v1"));
    assert!(affected_versions.contains("v2"));
    assert_eq!(damaged_report.healthy_versions.len(), 0);

    let vault_damaged_before = compute_directory_tree_hash(&vault_dir);
    let _ = execute_verification(&store, &db).unwrap();
    let vault_damaged_after = compute_directory_tree_hash(&vault_dir);
    assert_eq!(vault_damaged_before, vault_damaged_after);
}

fn compute_directory_tree_hash(root: &std::path::Path) -> String {
    let mut files = Vec::new();
    for entry in WalkDir::new(root).sort_by_file_name() {
        let entry = entry.unwrap();
        if entry.file_type().is_file() {
            let path = entry.path().to_str().unwrap().to_string();
            let content = fs::read(entry.path()).unwrap();
            let hash = sha256_hex(&content);
            files.push(format!("{}:{}", path, hash));
        }
    }
    sha256_hex(files.join("\n").as_bytes())
}
