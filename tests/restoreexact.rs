use brokenvault::client::restore::{RestoreOptions, run_restore};
use brokenvault::core::hash::sha256_hex;
use brokenvault::core::manifest::{Manifest, ManifestEntry};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use tempfile::tempdir;

type Hook = Box<dyn Fn() + Send + Sync>;

struct TestServer {
    url: String,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl TestServer {
    fn start(manifest: Manifest, chunk: Option<(String, Vec<u8>)>, hook: Hook) -> Option<Self> {
        let listener = match TcpListener::bind("127.0.0.1:0") {
            Ok(listener) => listener,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("skipping network test: {e}");
                return None;
            }
            Err(e) => panic!("failed to bind test server: {e}"),
        };
        let url = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::clone(&stop);
        let manifest = manifest.canonical_bytes().unwrap();
        let worker = thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
                        let Ok(cloned) = stream.try_clone() else {
                            continue;
                        };
                        let mut reader = BufReader::new(cloned);
                        let mut request = String::new();
                        if reader.read_line(&mut request).is_err() {
                            continue;
                        }
                        loop {
                            let mut line = String::new();
                            if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                                break;
                            }
                        }
                        let (status, body) = if request.contains("/manifest ") {
                            ("200 OK", manifest.as_slice())
                        } else if let Some((id, data)) = &chunk {
                            if request.contains(&format!("/chunks/{} ", id)) {
                                hook();
                                ("200 OK", data.as_slice())
                            } else {
                                ("404 Not Found", &[][..])
                            }
                        } else {
                            ("404 Not Found", &[][..])
                        };
                        if write!(
                            stream,
                            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .is_ok()
                        {
                            let _ = stream.write_all(body);
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Some(Self {
            url,
            stop,
            worker: Some(worker),
        })
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run(dest: &Path, url: &str) -> Result<(), brokenvault::core::errors::CoreError> {
    run_restore("v7", dest, url, RestoreOptions { jobs: 1 })
}

#[test]
fn restores_empty_and_nested_directory_mtimes_into_existing_empty_destination() {
    let tmp = tempdir().unwrap();
    let dest = tmp.path().join("dest");
    std::fs::create_dir(&dest).unwrap();
    let empty_time = (1_600_000_000, 123_456_789);
    let parent_time = (1_500_000_000, 987_654_321);
    let manifest = Manifest::new(vec![
        ManifestEntry::Dir {
            path: "empty".into(),
            mtime: empty_time,
        },
        ManifestEntry::Dir {
            path: "parent".into(),
            mtime: parent_time,
        },
        ManifestEntry::Dir {
            path: "parent/child".into(),
            mtime: empty_time,
        },
        ManifestEntry::File {
            path: "parent/child/zero".into(),
            size: 0,
            mtime: parent_time,
            chunks: vec![],
        },
    ])
    .unwrap();
    let Some(server) = TestServer::start(manifest, None, Box::new(|| {})) else {
        return;
    };
    run(&dest, &server.url).unwrap();
    for (path, (seconds, nanos)) in [
        ("empty", empty_time),
        ("parent", parent_time),
        ("parent/child", empty_time),
        ("parent/child/zero", parent_time),
    ] {
        let time = filetime::FileTime::from_last_modification_time(
            &std::fs::metadata(dest.join(path)).unwrap(),
        );
        assert_eq!(
            (time.unix_seconds(), time.nanoseconds()),
            (seconds, nanos),
            "{path}"
        );
    }
}

#[test]
fn file_mtime_error_is_reported_and_owned_file_is_cleaned() {
    let tmp = tempdir().unwrap();
    let dest = tmp.path().join("dest");
    std::fs::create_dir(&dest).unwrap();
    let manifest = Manifest::new(vec![ManifestEntry::File {
        path: "bad-file".into(),
        size: 0,
        mtime: (0, 2_000_000_000),
        chunks: vec![],
    }])
    .unwrap();
    let Some(server) = TestServer::start(manifest, None, Box::new(|| {})) else {
        return;
    };
    let err = run(&dest, &server.url).unwrap_err().to_string();
    assert!(
        err.contains("v7") && err.contains("bad-file") && err.contains("mtime"),
        "{err}"
    );
    assert!(dest.exists());
    assert!(!dest.join("bad-file").exists());
}

#[test]
fn directory_mtime_error_is_reported_and_owned_dir_is_cleaned() {
    let tmp = tempdir().unwrap();
    let dest = tmp.path().join("dest");
    let manifest = Manifest::new(vec![ManifestEntry::Dir {
        path: "bad-dir".into(),
        mtime: (0, 2_000_000_000),
    }])
    .unwrap();
    let Some(server) = TestServer::start(manifest, None, Box::new(|| {})) else {
        return;
    };
    let err = run(&dest, &server.url).unwrap_err().to_string();
    assert!(
        err.contains("v7") && err.contains("bad-dir") && err.contains("mtime"),
        "{err}"
    );
    assert!(!dest.exists());
}

#[test]
fn missing_chunk_reports_version_path_and_chunk_and_only_cleans_own_entries() {
    let tmp = tempdir().unwrap();
    let dest = tmp.path().join("missing").join("dest");
    let id = sha256_hex(b"payload");
    let manifest = Manifest::new(vec![ManifestEntry::File {
        path: "sub/file".into(),
        size: 7,
        mtime: (0, 0),
        chunks: vec![(id.clone(), 7)],
    }])
    .unwrap();
    let Some(server) = TestServer::start(manifest, None, Box::new(|| {})) else {
        return;
    };
    let err = run(&dest, &server.url).unwrap_err().to_string();
    assert!(
        err.contains("v7") && err.contains("sub/file") && err.contains(&id),
        "{err}"
    );
    assert!(
        !tmp.path().join("missing").exists(),
        "failed restore should remove only its own newly created directories"
    );
}

#[test]
fn failure_in_existing_empty_destination_preserves_new_user_data() {
    let tmp = tempdir().unwrap();
    let dest = tmp.path().join("dest");
    std::fs::create_dir(&dest).unwrap();
    let id = sha256_hex(b"payload");
    let manifest = Manifest::new(vec![ManifestEntry::File {
        path: "file".into(),
        size: 7,
        mtime: (0, 0),
        chunks: vec![(id, 7)],
    }])
    .unwrap();
    let user_file = dest.join("user-data");
    let Some(server) = TestServer::start(
        manifest,
        Some((sha256_hex(b"payload"), b"wrong".to_vec())),
        Box::new(move || {
            std::fs::write(&user_file, b"keep me").unwrap();
        }),
    ) else {
        return;
    };
    assert!(run(&dest, &server.url).is_err());
    assert_eq!(std::fs::read(dest.join("user-data")).unwrap(), b"keep me");
    assert!(!dest.join("file").exists());
}

#[cfg(unix)]
#[test]
fn symlink_destination_and_parent_are_rejected_without_touching_target() {
    use std::os::unix::fs::symlink;
    let tmp = tempdir().unwrap();
    let outside = tmp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("keep"), b"safe").unwrap();
    let link = tmp.path().join("link");
    symlink(&outside, &link).unwrap();
    let err = run(&link, "http://127.0.0.1:1").unwrap_err().to_string();
    assert!(err.contains("v7") && err.contains("destination"), "{err}");
    assert!(run(&link.join("child"), "http://127.0.0.1:1").is_err());
    assert_eq!(std::fs::read(outside.join("keep")).unwrap(), b"safe");
    assert!(!outside.join("child").exists());
    std::fs::create_dir(outside.join("empty")).unwrap();
    assert!(run(&link.join("empty"), "http://127.0.0.1:1").is_err());
    assert!(
        std::fs::read_dir(outside.join("empty"))
            .unwrap()
            .next()
            .is_none()
    );
    std::fs::create_dir(outside.join("nested")).unwrap();
    std::fs::create_dir(outside.join("nested/existing")).unwrap();
    assert!(run(&link.join("nested/existing"), "http://127.0.0.1:1").is_err());
    assert!(run(&link.join("nested/new"), "http://127.0.0.1:1").is_err());
    assert!(!outside.join("nested/new").exists());
}

#[cfg(unix)]
#[test]
fn replacement_symlink_cannot_redirect_file_metadata_or_cleanup() {
    use std::os::unix::fs::symlink;
    let tmp = tempdir().unwrap();
    let dest = tmp.path().join("dest");
    let outside = tmp.path().join("outside");
    std::fs::write(&outside, b"safe").unwrap();
    let id = sha256_hex(b"data");
    let manifest = Manifest::new(vec![
        ManifestEntry::File {
            path: "a-empty".into(),
            size: 0,
            mtime: (0, 0),
            chunks: vec![],
        },
        ManifestEntry::File {
            path: "z-chunk".into(),
            size: 4,
            mtime: (0, 0),
            chunks: vec![(id.clone(), 4)],
        },
    ])
    .unwrap();
    let file = dest.join("a-empty");
    let outside_for_hook = outside.clone();
    let Some(server) = TestServer::start(
        manifest,
        Some((id, b"data".to_vec())),
        Box::new(move || {
            std::fs::remove_file(&file).unwrap();
            symlink(&outside_for_hook, &file).unwrap();
        }),
    ) else {
        return;
    };
    let err = run(&dest, &server.url).unwrap_err().to_string();
    assert!(
        err.contains("v7") && err.contains("a-empty") && err.contains("mtime"),
        "{err}"
    );
    assert_eq!(std::fs::read(&outside).unwrap(), b"safe");
    assert!(
        std::fs::symlink_metadata(dest.join("a-empty"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(!dest.join("z-chunk").exists());
}

#[cfg(unix)]
#[test]
fn replacement_parent_symlink_cannot_redirect_chunk_write_or_cleanup() {
    use std::os::unix::fs::symlink;
    let tmp = tempdir().unwrap();
    let dest = tmp.path().join("dest");
    let outside = tmp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("file"), b"safe").unwrap();
    let id = sha256_hex(b"data");
    let manifest = Manifest::new(vec![ManifestEntry::File {
        path: "parent/file".into(),
        size: 4,
        mtime: (0, 0),
        chunks: vec![(id.clone(), 4)],
    }])
    .unwrap();
    let parent = dest.join("parent");
    let outside_for_hook = outside.clone();
    let Some(server) = TestServer::start(
        manifest,
        Some((id.clone(), b"data".to_vec())),
        Box::new(move || {
            std::fs::remove_file(parent.join("file")).unwrap();
            std::fs::remove_dir(&parent).unwrap();
            symlink(&outside_for_hook, &parent).unwrap();
        }),
    ) else {
        return;
    };
    let err = run(&dest, &server.url).unwrap_err().to_string();
    assert!(
        err.contains("v7") && err.contains("parent/file") && err.contains(&id),
        "{err}"
    );
    assert_eq!(std::fs::read(outside.join("file")).unwrap(), b"safe");
    assert!(
        std::fs::symlink_metadata(dest.join("parent"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[cfg(unix)]
#[test]
fn replacement_directory_symlink_cannot_redirect_mtime_or_cleanup() {
    use std::os::unix::fs::symlink;
    let tmp = tempdir().unwrap();
    let dest = tmp.path().join("dest");
    let outside = tmp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("keep"), b"safe").unwrap();
    let id = sha256_hex(b"data");
    let manifest = Manifest::new(vec![
        ManifestEntry::Dir {
            path: "empty".into(),
            mtime: (0, 0),
        },
        ManifestEntry::File {
            path: "trigger".into(),
            size: 4,
            mtime: (0, 0),
            chunks: vec![(id.clone(), 4)],
        },
    ])
    .unwrap();
    let dir = dest.join("empty");
    let outside_for_hook = outside.clone();
    let Some(server) = TestServer::start(
        manifest,
        Some((id, b"data".to_vec())),
        Box::new(move || {
            std::fs::remove_dir(&dir).unwrap();
            symlink(&outside_for_hook, &dir).unwrap();
        }),
    ) else {
        return;
    };
    let err = run(&dest, &server.url).unwrap_err().to_string();
    assert!(
        err.contains("v7") && err.contains("empty") && err.contains("mtime"),
        "{err}"
    );
    assert_eq!(std::fs::read(outside.join("keep")).unwrap(), b"safe");
    assert!(
        std::fs::symlink_metadata(dest.join("empty"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(!dest.join("trigger").exists());
}
