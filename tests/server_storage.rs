use brokenvault::core::chunker::chunk_slice;
use brokenvault::core::manifest::{Manifest, ManifestEntry};
use brokenvault::server::commit::{CommitOutcome, execute_commit};
use brokenvault::server::db::Database;
use brokenvault::server::store::Store;
use tempfile::tempdir;

#[test]
fn test_store_atomic_write_and_cleanup() {
    let tmp = tempdir().unwrap();
    let store = Store::new(tmp.path()).unwrap();

    let data = b"sample chunk data for testing storage";
    let chunks = chunk_slice(data).unwrap();
    let chunk_id = &chunks[0].hash;
    let len = chunks[0].len;

    assert!(!store.has_chunk(chunk_id, len));

    let tmp_path = store.write_chunk_tmp(chunk_id, data).unwrap();
    assert!(tmp_path.exists());
    assert!(!store.has_chunk(chunk_id, len));

    let promoted = store.promote_tmp_chunk(&tmp_path, chunk_id).unwrap();
    assert!(promoted.exists());
    assert!(!tmp_path.exists());
    assert!(store.has_chunk(chunk_id, len));
    assert!(!store.has_chunk(chunk_id, len + 1));

    let store2 = Store::new(tmp.path()).unwrap();
    assert!(store2.has_chunk(chunk_id, len));
}

#[test]
fn test_database_ledger_and_versions() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("meta.db");
    let mut db = Database::open(&db_path).unwrap();

    let vault_id = db.vault_id().unwrap();
    assert!(vault_id.starts_with("vlt_"));

    let manifest_bytes = b"{\"format\":\"brokenvault-manifest/1\"}";
    let manifest_id = "man_test_123";
    db.store_manifest(manifest_id, manifest_bytes).unwrap();

    let upload = db.create_upload("up_1", manifest_id, 1000).unwrap();
    assert_eq!(upload.id, "up_1");
    assert_eq!(upload.state, "open");

    let found = db.find_open_upload_by_manifest(manifest_id).unwrap();
    assert_eq!(found.unwrap().id, "up_1");

    assert!(db.list_versions().unwrap().is_empty());

    db.record_accepted_chunk("up_1", "chk_a", 400).unwrap();
    db.record_accepted_chunk("up_1", "chk_b", 600).unwrap();
    db.record_accepted_chunk("up_1", "chk_a", 400).unwrap();

    assert_eq!(db.get_uploaded_bytes("up_1").unwrap(), 1000);

    let version = db
        .commit_transaction("up_1", manifest_id, 1000, 1, 0, 2)
        .unwrap();
    assert_eq!(version.id, "v1");
    assert_eq!(version.uploaded_bytes, 1000);
    assert_eq!(version.reused_bytes, 0);

    let versions = db.list_versions().unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].id, "v1");

    assert!(
        db.find_open_upload_by_manifest(manifest_id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn test_commit_barrier_validation_and_quarantine() {
    let tmp = tempdir().unwrap();
    let vault_dir = tmp.path().join("vault");
    let store = Store::new(&vault_dir).unwrap();
    let mut db = Database::open(vault_dir.join("meta.db")).unwrap();

    let data = b"brokenvault commit barrier test content";
    let chunks = chunk_slice(data).unwrap();
    let chunk_id = &chunks[0].hash;
    let chunk_len = chunks[0].len;

    let entries = vec![ManifestEntry::File {
        path: "data.bin".to_string(),
        size: chunk_len,
        mtime: (1000, 0),
        chunks: vec![(chunk_id.clone(), chunk_len)],
    }];
    let manifest = Manifest::new(entries).unwrap();
    let manifest_bytes = manifest.canonical_bytes().unwrap();
    let manifest_id = manifest.manifest_id().unwrap();

    db.store_manifest(&manifest_id, &manifest_bytes).unwrap();
    db.create_upload("up_barrier", &manifest_id, chunk_len)
        .unwrap();

    let outcome_missing = execute_commit(&store, &mut db, "up_barrier").unwrap();
    match outcome_missing {
        CommitOutcome::Conflict { missing_or_corrupt } => {
            assert_eq!(missing_or_corrupt, vec![chunk_id.clone()]);
        }
        other => panic!("expected conflict, got {:?}", other),
    }

    let corrupt_data = b"tampered byte sequence";
    let tmp_path = store.write_chunk_tmp(chunk_id, corrupt_data).unwrap();
    store.promote_tmp_chunk(&tmp_path, chunk_id).unwrap();

    let outcome_corrupt = execute_commit(&store, &mut db, "up_barrier").unwrap();
    match outcome_corrupt {
        CommitOutcome::Conflict { missing_or_corrupt } => {
            assert_eq!(missing_or_corrupt, vec![chunk_id.clone()]);
        }
        other => panic!("expected conflict, got {:?}", other),
    }

    assert!(!store.chunk_path(chunk_id).exists());

    let tmp_path_correct = store.write_chunk_tmp(chunk_id, data).unwrap();
    store
        .promote_tmp_chunk(&tmp_path_correct, chunk_id)
        .unwrap();
    db.record_accepted_chunk("up_barrier", chunk_id, chunk_len)
        .unwrap();

    let outcome_success = execute_commit(&store, &mut db, "up_barrier").unwrap();
    let v_id = match outcome_success {
        CommitOutcome::Success(v) => {
            assert_eq!(v.id, "v1");
            v.id
        }
        other => panic!("expected success, got {:?}", other),
    };

    let outcome_already = execute_commit(&store, &mut db, "up_barrier").unwrap();
    match outcome_already {
        CommitOutcome::AlreadyCommitted(v) => {
            assert_eq!(v.id, v_id);
        }
        other => panic!("expected already committed, got {:?}", other),
    }
}
