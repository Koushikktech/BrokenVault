use brokenvault::client::devtools::{diff_directories, generate_sample_dataset};
use brokenvault::client::journal::{Journal, JournalEntry, make_journal_key};
use brokenvault::client::scan::scan_directory;
use tempfile::tempdir;

#[test]
fn test_scan_directory_and_manifest() {
    let tmp = tempdir().unwrap();
    let src = tmp.path().join("dataset");

    generate_sample_dataset(&src, 1234, false).unwrap();

    let (manifest, chunk_details) = scan_directory(&src).unwrap();

    assert!(manifest.total_files() >= 5);
    assert!(manifest.total_dirs() >= 3);
    assert!(manifest.total_bytes() > 4 * 1024 * 1024);
    assert!(!chunk_details.is_empty());

    let empty_entry = manifest
        .entries
        .iter()
        .find(|e| e.path() == "empty.bin")
        .unwrap();
    assert_eq!(empty_entry.path(), "empty.bin");

    let exports_entry = manifest
        .entries
        .iter()
        .find(|e| e.path() == "exports")
        .unwrap();
    assert_eq!(exports_entry.path(), "exports");
}

#[test]
fn test_devtools_diff_and_mutate() {
    let tmp = tempdir().unwrap();
    let dir_a = tmp.path().join("dir_a");
    let dir_b = tmp.path().join("dir_b");

    generate_sample_dataset(&dir_a, 42, false).unwrap();
    generate_sample_dataset(&dir_b, 42, false).unwrap();

    assert!(diff_directories(&dir_a, &dir_b).unwrap());

    generate_sample_dataset(&dir_b, 42, true).unwrap();

    assert!(!diff_directories(&dir_a, &dir_b).unwrap());
}

#[test]
fn test_journal_roundtrip() {
    let tmp = tempdir().unwrap();
    let state_dir = tmp.path().join("state");
    unsafe {
        std::env::set_var("BV_STATE_DIR", &state_dir);
    }

    let mut journal = Journal::load_from_disk();
    let key = make_journal_key("http://127.0.0.1:7878", "man_abc");

    journal.insert(
        key.clone(),
        JournalEntry {
            upload_id: "upl_123".to_string(),
            server_url: "http://127.0.0.1:7878".to_string(),
            manifest_id: "man_abc".to_string(),
            created_at: 1000,
            committed: false,
            version_id: None,
        },
    );

    journal.save_to_disk().unwrap();

    let reloaded = Journal::load_from_disk();
    let entry = reloaded.get(&key).unwrap();
    assert_eq!(entry.upload_id, "upl_123");
    assert!(!entry.committed);
}
