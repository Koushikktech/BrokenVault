use crate::core::errors::CoreError;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadRow {
    pub id: String,
    pub manifest_id: String,
    pub total_bytes: u64,
    pub created_at: i64,
    pub state: String,
    pub version_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionRow {
    pub seq: i64,
    pub id: String,
    pub upload_id: String,
    pub manifest_id: String,
    pub committed_at: i64,
    pub total_bytes: u64,
    pub uploaded_bytes: u64,
    pub reused_bytes: u64,
    pub files: usize,
    pub dirs: usize,
    pub chunks: usize,
}

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self, CoreError> {
        let conn = Connection::open(db_path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = FULL;
             PRAGMA busy_timeout = 5000;",
        )?;

        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    pub fn open_read_only(db_path: impl AsRef<Path>) -> Result<Self, CoreError> {
        let path = db_path.as_ref().canonicalize()?;
        let mut wal = path.as_os_str().to_os_string();
        wal.push("-wal");
        let mut shm = path.as_os_str().to_os_string();
        shm.push("-shm");
        let wal_present = PathBuf::from(wal).exists();
        let shm_present = PathBuf::from(shm).exists();
        if wal_present && !shm_present {
            return Err(CoreError::Io(std::io::Error::other(
                "cannot verify WAL database without existing shared-memory file in read-only mode",
            )));
        }
        let conn = if wal_present {
            Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?
        } else {
            let path = path.to_str().ok_or_else(|| {
                CoreError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "database path is not UTF-8",
                ))
            })?;
            let uri = format!(
                "file:{}?immutable=1",
                path.replace('%', "%25")
                    .replace('?', "%3F")
                    .replace('#', "%23")
            );
            Connection::open_with_flags(
                uri,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
            )?
        };
        Ok(Self { conn })
    }

    fn init_schema(&self) -> Result<(), CoreError> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS manifests (
                manifest_id TEXT PRIMARY KEY,
                bytes BLOB NOT NULL
            );

            CREATE TABLE IF NOT EXISTS uploads (
                id TEXT PRIMARY KEY,
                manifest_id TEXT NOT NULL REFERENCES manifests(manifest_id),
                total_bytes INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                state TEXT NOT NULL CHECK(state IN ('open', 'committed', 'aborted')),
                version_id TEXT
            );

            CREATE INDEX IF NOT EXISTS uploads_open ON uploads(manifest_id) WHERE state = 'open';

            CREATE TABLE IF NOT EXISTS accepted (
                upload_id TEXT NOT NULL,
                chunk_id TEXT NOT NULL,
                len INTEGER NOT NULL,
                PRIMARY KEY(upload_id, chunk_id)
            ) WITHOUT ROWID;

            CREATE TABLE IF NOT EXISTS versions (
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                id TEXT UNIQUE NOT NULL,
                upload_id TEXT UNIQUE NOT NULL,
                manifest_id TEXT NOT NULL REFERENCES manifests(manifest_id),
                committed_at INTEGER NOT NULL,
                total_bytes INTEGER NOT NULL,
                uploaded_bytes INTEGER NOT NULL,
                reused_bytes INTEGER NOT NULL,
                files INTEGER NOT NULL,
                dirs INTEGER NOT NULL,
                chunks INTEGER NOT NULL
            );",
        )?;

        let existing_vault_id: Option<String> = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = 'vault_id'", [], |r| {
                r.get(0)
            })
            .optional()?;

        if existing_vault_id.is_none() {
            let rand_val: u64 = rand::random();
            let vault_id = format!("vlt_{:016x}", rand_val);
            self.conn.execute(
                "INSERT INTO meta (key, value) VALUES ('vault_id', ?)",
                params![vault_id],
            )?;
        }

        Ok(())
    }

    pub fn vault_id(&self) -> Result<String, CoreError> {
        let id = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = 'vault_id'", [], |r| {
                r.get(0)
            })?;
        Ok(id)
    }

    pub fn store_manifest(&self, manifest_id: &str, bytes: &[u8]) -> Result<(), CoreError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO manifests (manifest_id, bytes) VALUES (?, ?)",
            params![manifest_id, bytes],
        )?;
        Ok(())
    }

    pub fn get_manifest_bytes(&self, manifest_id: &str) -> Result<Option<Vec<u8>>, CoreError> {
        let bytes = self
            .conn
            .query_row(
                "SELECT bytes FROM manifests WHERE manifest_id = ?",
                params![manifest_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(bytes)
    }

    pub fn find_open_upload_by_manifest(
        &self,
        manifest_id: &str,
    ) -> Result<Option<UploadRow>, CoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT id, manifest_id, total_bytes, created_at, state, version_id
                 FROM uploads
                 WHERE manifest_id = ? AND state = 'open'
                 ORDER BY created_at DESC LIMIT 1",
                params![manifest_id],
                |r| {
                    Ok(UploadRow {
                        id: r.get(0)?,
                        manifest_id: r.get(1)?,
                        total_bytes: r.get(2)?,
                        created_at: r.get(3)?,
                        state: r.get(4)?,
                        version_id: r.get(5)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    pub fn create_upload(
        &self,
        id: &str,
        manifest_id: &str,
        total_bytes: u64,
    ) -> Result<UploadRow, CoreError> {
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(std::io::Error::other)?
            .as_secs() as i64;

        self.conn.execute(
            "INSERT INTO uploads (id, manifest_id, total_bytes, created_at, state, version_id)
             VALUES (?, ?, ?, ?, 'open', NULL)",
            params![id, manifest_id, total_bytes, created_at],
        )?;

        Ok(UploadRow {
            id: id.to_string(),
            manifest_id: manifest_id.to_string(),
            total_bytes,
            created_at,
            state: "open".to_string(),
            version_id: None,
        })
    }

    pub fn get_upload(&self, id: &str) -> Result<Option<UploadRow>, CoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT id, manifest_id, total_bytes, created_at, state, version_id
                 FROM uploads WHERE id = ?",
                params![id],
                |r| {
                    Ok(UploadRow {
                        id: r.get(0)?,
                        manifest_id: r.get(1)?,
                        total_bytes: r.get(2)?,
                        created_at: r.get(3)?,
                        state: r.get(4)?,
                        version_id: r.get(5)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    pub fn list_open_uploads(&self) -> Result<Vec<UploadRow>, CoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, manifest_id, total_bytes, created_at, state, version_id
             FROM uploads WHERE state = 'open' ORDER BY created_at ASC",
        )?;

        let rows = stmt.query_map([], |r| {
            Ok(UploadRow {
                id: r.get(0)?,
                manifest_id: r.get(1)?,
                total_bytes: r.get(2)?,
                created_at: r.get(3)?,
                state: r.get(4)?,
                version_id: r.get(5)?,
            })
        })?;

        let mut uploads = Vec::new();
        for row in rows {
            uploads.push(row?);
        }
        Ok(uploads)
    }

    pub fn record_accepted_chunk(
        &self,
        upload_id: &str,
        chunk_id: &str,
        len: u64,
    ) -> Result<bool, CoreError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO accepted (upload_id, chunk_id, len)
             SELECT ?, ?, ? WHERE EXISTS (
                 SELECT 1 FROM uploads WHERE id = ? AND state = 'open'
             )",
            params![upload_id, chunk_id, len, upload_id],
        )?;
        self.is_upload_open(upload_id)
    }

    pub fn is_upload_open(&self, upload_id: &str) -> Result<bool, CoreError> {
        let open: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM uploads WHERE id = ? AND state = 'open')",
            params![upload_id],
            |r| r.get(0),
        )?;
        Ok(open)
    }

    pub fn get_uploaded_bytes(&self, upload_id: &str) -> Result<u64, CoreError> {
        let sum: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(len), 0) FROM accepted WHERE upload_id = ?",
            params![upload_id],
            |r| r.get(0),
        )?;
        Ok(sum as u64)
    }

    pub fn abort_upload(&self, upload_id: &str) -> Result<bool, CoreError> {
        let affected = self.conn.execute(
            "UPDATE uploads SET state = 'aborted' WHERE id = ? AND state = 'open'",
            params![upload_id],
        )?;
        Ok(affected > 0)
    }

    pub fn commit_transaction(
        &mut self,
        upload_id: &str,
        manifest_id: &str,
        total_bytes: u64,
        files: usize,
        dirs: usize,
        chunks: usize,
    ) -> Result<VersionRow, CoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        let state: String = tx.query_row(
            "SELECT state FROM uploads WHERE id = ?",
            params![upload_id],
            |r| r.get(0),
        )?;

        if state != "open" {
            return Err(CoreError::Io(std::io::Error::other(format!(
                "upload {} is not open (current state: {})",
                upload_id, state
            ))));
        }

        let uploaded_bytes_i64: i64 = tx.query_row(
            "SELECT COALESCE(SUM(len), 0) FROM accepted WHERE upload_id = ?",
            params![upload_id],
            |r| r.get(0),
        )?;
        let uploaded_bytes = uploaded_bytes_i64 as u64;
        let reused_bytes = total_bytes.saturating_sub(uploaded_bytes);

        let next_seq: i64 =
            tx.query_row("SELECT COALESCE(MAX(seq), 0) + 1 FROM versions", [], |r| {
                r.get(0)
            })?;
        let version_id = format!("v{}", next_seq);

        let committed_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(std::io::Error::other)?
            .as_secs() as i64;

        tx.execute(
            "INSERT INTO versions (seq, id, upload_id, manifest_id, committed_at, total_bytes, uploaded_bytes, reused_bytes, files, dirs, chunks)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                next_seq,
                version_id,
                upload_id,
                manifest_id,
                committed_at,
                total_bytes,
                uploaded_bytes,
                reused_bytes,
                files as i64,
                dirs as i64,
                chunks as i64,
            ],
        )?;

        tx.execute(
            "UPDATE uploads SET state = 'committed', version_id = ? WHERE id = ?",
            params![version_id, upload_id],
        )?;

        tx.commit()?;

        Ok(VersionRow {
            seq: next_seq,
            id: version_id,
            upload_id: upload_id.to_string(),
            manifest_id: manifest_id.to_string(),
            committed_at,
            total_bytes,
            uploaded_bytes,
            reused_bytes,
            files,
            dirs,
            chunks,
        })
    }

    pub fn list_versions(&self) -> Result<Vec<VersionRow>, CoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, id, upload_id, manifest_id, committed_at, total_bytes, uploaded_bytes, reused_bytes, files, dirs, chunks
             FROM versions ORDER BY seq ASC",
        )?;

        let rows = stmt.query_map([], |r| {
            Ok(VersionRow {
                seq: r.get(0)?,
                id: r.get(1)?,
                upload_id: r.get(2)?,
                manifest_id: r.get(3)?,
                committed_at: r.get(4)?,
                total_bytes: r.get(5)?,
                uploaded_bytes: r.get(6)?,
                reused_bytes: r.get(7)?,
                files: r.get::<_, i64>(8)? as usize,
                dirs: r.get::<_, i64>(9)? as usize,
                chunks: r.get::<_, i64>(10)? as usize,
            })
        })?;

        let mut list = Vec::new();
        for row in rows {
            list.push(row?);
        }
        Ok(list)
    }

    pub fn get_version(&self, version_id: &str) -> Result<Option<VersionRow>, CoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT seq, id, upload_id, manifest_id, committed_at, total_bytes, uploaded_bytes, reused_bytes, files, dirs, chunks
                 FROM versions WHERE id = ?",
                params![version_id],
                |r| {
                    Ok(VersionRow {
                        seq: r.get(0)?,
                        id: r.get(1)?,
                        upload_id: r.get(2)?,
                        manifest_id: r.get(3)?,
                        committed_at: r.get(4)?,
                        total_bytes: r.get(5)?,
                        uploaded_bytes: r.get(6)?,
                        reused_bytes: r.get(7)?,
                        files: r.get::<_, i64>(8)? as usize,
                        dirs: r.get::<_, i64>(9)? as usize,
                        chunks: r.get::<_, i64>(10)? as usize,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    pub fn get_version_manifest(&self, version_id: &str) -> Result<Option<Vec<u8>>, CoreError> {
        let bytes = self
            .conn
            .query_row(
                "SELECT m.bytes FROM manifests m
                 JOIN versions v ON v.manifest_id = m.manifest_id
                 WHERE v.id = ?",
                params![version_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(bytes)
    }

    pub fn list_completed_manifests(&self) -> Result<Vec<(String, Vec<u8>)>, CoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT v.id, m.bytes FROM versions v
             JOIN manifests m ON v.manifest_id = m.manifest_id
             ORDER BY v.seq ASC",
        )?;

        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;

        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }
}
