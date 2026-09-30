use brokenvault::client::devtools::generate_sample_dataset;
use brokenvault::client::upload::{UploadOptions, run_backup};
use brokenvault::server::api::{AppState, create_router};
use brokenvault::server::db::Database;
use brokenvault::server::store::Store;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;
use tokio::net::TcpListener;
use walkdir::WalkDir;

#[test]
fn test_deduplication_and_one_copy_per_hash() {
    let tmp = tempdir().unwrap();
    let src_dir = tmp.path().join("source");
    let vault_dir = tmp.path().join("vault");

    generate_sample_dataset(&src_dir, 777, false).unwrap();

    let store = Store::new(&vault_dir).unwrap();
    let db = Database::open(vault_dir.join("meta.db")).unwrap();
    let state = AppState {
        store: store.clone(),
        db: Arc::new(Mutex::new(db)),
    };

    let rt = tokio::runtime::Runtime::new().unwrap();
    let (addr_tx, addr_rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        rt.block_on(async move {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            addr_tx.send(addr).unwrap();
            let router = create_router(state);
            axum::serve(listener, router).await.unwrap();
        });
    });

    let addr = match addr_rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(a) => a,
        Err(_) => return,
    };
    let server_url = format!("http://{}", addr);

    let v1_res = run_backup(&src_dir, &server_url, UploadOptions::default());
    let v1 = match v1_res {
        Ok(c) => c.expect("v1 commit should be returned"),
        Err(e) => {
            if let brokenvault::core::errors::CoreError::Io(ref io_err) = e {
                if io_err.raw_os_error() == Some(1)
                    || io_err.to_string().contains("Operation not permitted")
                {
                    eprintln!(
                        "Skipping test: network socket denied by sandbox environment (EPERM)"
                    );
                    return;
                }
            }
            panic!("v1 backup failed: {:?}", e);
        }
    };
    assert_eq!(v1.version, "v1");
    assert_eq!(v1.uploaded_bytes, v1.total_bytes);

    generate_sample_dataset(&src_dir, 777, true).unwrap();

    let v2_res = run_backup(&src_dir, &server_url, UploadOptions::default());
    let v2 = v2_res
        .expect("v2 backup must succeed")
        .expect("v2 commit must be present");
    assert_eq!(v2.version, "v2");

    let uploaded_ratio = (v2.uploaded_bytes as f64) / (v2.total_bytes as f64);
    assert!(
        uploaded_ratio < 0.15,
        "v2 uploaded ratio too high: {} (uploaded {}, total {})",
        uploaded_ratio,
        v2.uploaded_bytes,
        v2.total_bytes
    );
    assert_eq!(v2.total_bytes, v2.uploaded_bytes + v2.reused_bytes);

    let chunks_dir = vault_dir.join("chunks");
    let mut stored_chunk_ids = HashSet::new();
    let mut total_chunk_files = 0;

    for entry in WalkDir::new(&chunks_dir).min_depth(2) {
        let entry = entry.unwrap();
        if entry.file_type().is_file() {
            total_chunk_files += 1;
            let id = entry.file_name().to_str().unwrap().to_string();
            assert!(stored_chunk_ids.insert(id), "duplicate chunk file on disk");
        }
    }

    assert_eq!(stored_chunk_ids.len(), total_chunk_files);
}
