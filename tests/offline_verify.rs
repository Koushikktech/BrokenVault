use brokenvault::core::hash::sha256_hex;
use brokenvault::core::manifest::{Manifest, ManifestEntry};
use brokenvault::server::commit::execute_commit;
use brokenvault::server::db::Database;
use brokenvault::server::store::Store;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

fn run_verify(data: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_bvd"))
        .args(["verify", "--data", data.to_str().unwrap(), "--json"])
        .output()
        .unwrap()
}

#[test]
fn offline_verify_is_read_only_and_reports_correct_exit_status() {
    let tmp = tempdir().unwrap();
    let vault = tmp.path().join("vault");
    let store = Store::new(&vault).unwrap();
    let mut db = Database::open(vault.join("meta.db")).unwrap();
    let data = b"verify this chunk";
    let id = sha256_hex(data);
    let manifest = Manifest::new(vec![ManifestEntry::File {
        path: "file".to_string(),
        size: data.len() as u64,
        mtime: (0, 0),
        chunks: vec![(id.clone(), data.len() as u64)],
    }])
    .unwrap();
    let manifest_id = manifest.manifest_id().unwrap();
    db.store_manifest(&manifest_id, &manifest.canonical_bytes().unwrap())
        .unwrap();
    db.create_upload("upload", &manifest_id, data.len() as u64)
        .unwrap();
    let tmp_chunk = store.write_chunk_tmp(&id, data).unwrap();
    store.promote_tmp_chunk(&tmp_chunk, &id).unwrap();
    execute_commit(&store, &mut db, "upload").unwrap();
    let wal_snapshot = || {
        ["meta.db", "meta.db-wal"]
            .into_iter()
            .map(|name| fs::read(vault.join(name)).ok())
            .collect::<Vec<_>>()
    };
    let before_wal_read = wal_snapshot();
    assert!(before_wal_read[1].is_some());
    assert!(vault.join("meta.db-shm").exists());
    let while_wal_open = run_verify(&vault);
    assert!(
        while_wal_open.status.success(),
        "{}",
        String::from_utf8_lossy(&while_wal_open.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&while_wal_open.stdout).unwrap()["healthy_versions"],
        serde_json::json!(["v1"])
    );
    assert_eq!(before_wal_read, wal_snapshot());

    let incomplete = tmp.path().join("wal_without_shm");
    fs::create_dir(&incomplete).unwrap();
    fs::copy(vault.join("meta.db"), incomplete.join("meta.db")).unwrap();
    fs::copy(vault.join("meta.db-wal"), incomplete.join("meta.db-wal")).unwrap();
    let incomplete_before = fs::read(incomplete.join("meta.db-wal")).unwrap();
    assert!(!run_verify(&incomplete).status.success());
    assert!(!incomplete.join("meta.db-shm").exists());
    assert_eq!(
        incomplete_before,
        fs::read(incomplete.join("meta.db-wal")).unwrap()
    );
    drop(db);

    let marker = vault.join("tmp").join("keep.tmp");
    fs::write(&marker, b"leave me alone").unwrap();
    let snapshot = || {
        ["meta.db", "meta.db-wal", "tmp/keep.tmp"]
            .into_iter()
            .map(|name| (name.to_string(), fs::read(vault.join(name)).ok()))
            .collect::<Vec<_>>()
    };
    let before = snapshot();
    let healthy = run_verify(&vault);
    assert!(
        healthy.status.success(),
        "{}",
        String::from_utf8_lossy(&healthy.stderr)
    );
    assert!(
        serde_json::from_slice::<serde_json::Value>(&healthy.stdout).unwrap()["healthy"]
            .as_bool()
            .unwrap()
    );
    assert_eq!(before, snapshot());

    fs::remove_file(store.chunk_path(&id)).unwrap();
    let before_damage = snapshot();
    let damaged = run_verify(&vault);
    assert_eq!(
        damaged.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&damaged.stderr)
    );
    assert!(
        !serde_json::from_slice::<serde_json::Value>(&damaged.stdout).unwrap()["healthy"]
            .as_bool()
            .unwrap()
    );
    assert_eq!(before_damage, snapshot());

    let absent = tmp.path().join("missing");
    assert!(!run_verify(&absent).status.success());
    assert!(!absent.exists());
    let no_db = tmp.path().join("no_db");
    fs::create_dir(&no_db).unwrap();
    assert!(!run_verify(&no_db).status.success());
    assert_eq!(fs::read_dir(&no_db).unwrap().count(), 0);
}
