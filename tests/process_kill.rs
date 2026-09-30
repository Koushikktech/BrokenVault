#![cfg(unix)]

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{Method, StatusCode, Uri};
use axum::routing::any;
use brokenvault::client::devtools::{diff_directories, generate_sample_dataset};
use brokenvault::core::manifest::{Manifest, ManifestEntry};
use brokenvault::core::proto::{OpenUploadSummary, UploadStatusResponse, VersionSummary};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::net::TcpListener;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};
use tempfile::tempdir;
use tokio::sync::{Semaphore, oneshot};

struct Process(Child);
impl Process {
    fn kill_and_wait(&mut self) {
        assert!(
            self.0.try_wait().unwrap().is_none(),
            "process exited before SIGKILL"
        );
        self.0.kill().unwrap();
        let status = self.0.wait().unwrap();
        assert_eq!(status.signal(), Some(9), "process was not killed: {status}");
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[derive(Clone)]
struct ProxyState {
    backend: String,
    first_chunk: Arc<AtomicBool>,
    accepted: mpsc::Sender<()>,
    gate: Arc<Semaphore>,
    record_resume: Arc<AtomicBool>,
    resumed_puts: Arc<Mutex<Vec<(String, usize)>>>,
}

async fn forward(
    State(state): State<ProxyState>,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> (StatusCode, Bytes) {
    let path = uri.path().to_owned();
    let chunk_id = if method == Method::PUT {
        path.split("/chunks/").nth(1).map(str::to_owned)
    } else {
        None
    };
    let backend = state.backend.clone();
    let body_len = body.len();
    let response = tokio::task::spawn_blocking(move || {
        let client = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .new_agent();
        let url = format!("{backend}{path}");
        let result = match method {
            Method::GET => client.get(&url).call(),
            Method::PUT => client.put(&url).send(body.as_ref()),
            Method::POST => client.post(&url).send(body.as_ref()),
            _ => unreachable!("unexpected client request"),
        };
        let mut response = result.expect("proxy could not reach real bvd");
        let status = StatusCode::from_u16(response.status().as_u16()).unwrap();
        let bytes = response.body_mut().read_to_vec().unwrap();
        (status, Bytes::from(bytes))
    })
    .await
    .unwrap();
    if let Some(id) = chunk_id {
        if response.0.is_success() {
            if state.record_resume.load(Ordering::SeqCst) {
                state.resumed_puts.lock().unwrap().push((id, body_len));
            } else if !state.first_chunk.swap(true, Ordering::SeqCst) {
                state.accepted.send(()).unwrap();
                let _ = state.gate.acquire().await;
            }
        }
    }
    response
}

struct Proxy {
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    state: ProxyState,
}
impl Drop for Proxy {
    fn drop(&mut self) {
        self.state.gate.add_permits(1);
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn port() -> std::io::Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}
fn wait_health(url: &str, server: &mut Process) {
    let until = Instant::now() + Duration::from_secs(10);
    while Instant::now() < until {
        if let Ok(response) = ureq::get(&format!("{url}/v1/health")).call() {
            if response.status() == 200 {
                return;
            }
        }
        assert!(
            server.0.try_wait().unwrap().is_none(),
            "bvd exited before ready"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("bvd did not start on {url}");
}
fn server(vault: &Path, listen: &str) -> Process {
    Process(
        Command::new(env!("CARGO_BIN_EXE_bvd"))
            .args(["serve", "--data"])
            .arg(vault)
            .args(["--listen", listen])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    )
}
fn backup(src: &Path, state: &Path, proxy_url: &str, log: &Path) -> Process {
    Process(
        Command::new(env!("CARGO_BIN_EXE_bv"))
            .args(["--server", proxy_url, "backup"])
            .arg(src)
            .args(["--jobs", "1"])
            .env("BV_STATE_DIR", state)
            .stdout(fs::File::create(log).unwrap())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    )
}
fn json<T: serde::de::DeserializeOwned>(url: &str) -> T {
    let mut response = ureq::get(url).call().unwrap();
    assert_eq!(response.status(), 200);
    response.body_mut().read_json().unwrap()
}

#[test]
fn sigkill_bv_and_bvd_with_ack_in_flight_then_resume_only_missing_chunks() {
    let backend_port = match port() {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("SKIP process kill test: sandbox denies loopback bind: {e}");
            return;
        }
        Err(e) => panic!("loopback bind failed: {e}"),
    };
    let proxy_port = port().expect("proxy loopback bind");
    let tmp = tempdir().unwrap();
    let src = tmp.path().join("source");
    let vault = tmp.path().join("vault");
    let state_dir = tmp.path().join("state");
    let restored = tmp.path().join("restored");
    generate_sample_dataset(&src, 1001, false).unwrap();
    let backend_addr = format!("127.0.0.1:{backend_port}");
    let backend = format!("http://{backend_addr}");
    let proxy_url = format!("http://127.0.0.1:{proxy_port}");
    let mut bvd = server(&vault, &backend_addr);
    wait_health(&backend, &mut bvd);

    let (accepted_tx, accepted_rx) = mpsc::channel();
    let proxy_state = ProxyState {
        backend: backend.clone(),
        first_chunk: Arc::new(AtomicBool::new(false)),
        accepted: accepted_tx,
        gate: Arc::new(Semaphore::new(0)),
        record_resume: Arc::new(AtomicBool::new(false)),
        resumed_puts: Arc::new(Mutex::new(Vec::new())),
    };
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let (ready_tx, ready_rx) = mpsc::channel();
    let state_for_thread = proxy_state.clone();
    let proxy_thread = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{proxy_port}"))
                .await
                .unwrap();
            ready_tx.send(()).unwrap();
            axum::serve(
                listener,
                Router::new()
                    .fallback(any(forward))
                    .with_state(state_for_thread),
            )
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
        });
    });
    let proxy = Proxy {
        shutdown: Some(shutdown_tx),
        thread: Some(proxy_thread),
        state: proxy_state,
    };
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    let mut bv = backup(&src, &state_dir, &proxy_url, &tmp.path().join("first.log"));
    accepted_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("bv must upload at least one chunk");
    let open: Vec<OpenUploadSummary> = json(&format!("{backend}/v1/uploads"));
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].state, "open");
    assert!(
        open[0].chunks_present > 0 && open[0].chunks_present < open[0].chunks_total,
        "{open:?}"
    );
    let upload_id = open[0].upload_id.clone();
    let status: UploadStatusResponse = json(&format!("{backend}/v1/uploads/{upload_id}"));
    assert_eq!(status.state, "open");
    assert!(!status.missing.is_empty());
    let before_missing: HashSet<_> = status.missing.into_iter().collect();
    let versions: Vec<VersionSummary> = json(&format!("{backend}/v1/versions"));
    assert!(versions.is_empty(), "uncommitted upload was visible");

    bvd.kill_and_wait();
    bv.kill_and_wait();
    proxy.state.record_resume.store(true, Ordering::SeqCst);
    proxy.state.gate.add_permits(1);
    let mut bvd = server(&vault, &backend_addr);
    wait_health(&backend, &mut bvd);
    let open: Vec<OpenUploadSummary> = json(&format!("{backend}/v1/uploads"));
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].upload_id, upload_id);
    let after: UploadStatusResponse = json(&format!("{backend}/v1/uploads/{upload_id}"));
    assert_eq!(
        after.missing.into_iter().collect::<HashSet<_>>(),
        before_missing
    );
    assert!(json::<Vec<VersionSummary>>(&format!("{backend}/v1/versions")).is_empty());

    let second_log = tmp.path().join("second.log");
    let mut bv2 = backup(&src, &state_dir, &proxy_url, &second_log);
    let exit = bv2.0.wait().unwrap();
    let output = fs::read_to_string(&second_log).unwrap();
    assert!(exit.success(), "resume failed: {exit}: {output}");
    assert!(
        output.contains(&format!("Upload {upload_id} (resumed)")),
        "{output}"
    );
    let versions: Vec<VersionSummary> = json(&format!("{backend}/v1/versions"));
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].id, "v1");
    assert_eq!(versions[0].upload_id, upload_id);
    assert!(json::<Vec<OpenUploadSummary>>(&format!("{backend}/v1/uploads")).is_empty());
    let puts = proxy.state.resumed_puts.lock().unwrap().clone();
    let sent: HashSet<_> = puts.iter().map(|(id, _)| id.clone()).collect();
    assert_eq!(
        puts.len(),
        before_missing.len(),
        "resumed upload repeated a chunk"
    );
    assert_eq!(
        sent, before_missing,
        "resumed client must send exactly missing chunks"
    );
    let mut response = ureq::get(&format!("{backend}/v1/versions/v1/manifest"))
        .call()
        .unwrap();
    let manifest = Manifest::from_bytes(&response.body_mut().read_to_vec().unwrap()).unwrap();
    let lengths: HashMap<_, _> = manifest
        .entries
        .iter()
        .filter_map(|entry| match entry {
            ManifestEntry::File { chunks, .. } => Some(chunks.iter().cloned()),
            _ => None,
        })
        .flatten()
        .collect();
    let remaining_bytes: u64 = puts
        .iter()
        .map(|(id, len)| {
            assert_eq!(Some(&(*len as u64)), lengths.get(id));
            *len as u64
        })
        .sum();
    assert!(remaining_bytes > 0 && remaining_bytes < versions[0].total_bytes);
    assert_eq!(versions[0].uploaded_bytes, versions[0].total_bytes);
    assert_eq!(
        versions[0].uploaded_bytes - remaining_bytes,
        lengths
            .iter()
            .filter(|(id, _)| !before_missing.contains(*id))
            .map(|(_, len)| len)
            .sum::<u64>()
    );

    let restore = Command::new(env!("CARGO_BIN_EXE_bv"))
        .args(["--server", &proxy_url, "restore", "v1"])
        .arg(&restored)
        .status()
        .unwrap();
    assert!(restore.success());
    assert!(
        diff_directories(&src, &restored).unwrap(),
        "restored tree differs"
    );
    drop(proxy);
    bvd.kill_and_wait();
}
