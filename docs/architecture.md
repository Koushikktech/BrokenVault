# Architecture Note

BrokenVault is a deduplicated, content-defined backup and restore system built in Rust. It guarantees crash resilience, atomic version publication, and exact directory restoration.

```
 ┌──────────────── bv (client) ────────────────┐        HTTP/1.1         ┌────────────── bvd (server) ──────────────┐
 │ scan → chunk (FastCDC) → hash (SHA-256)     │  POST /v1/uploads       │ validate manifest → match open upload     │
 │ manifest (canonical JSON, sorted)           │ ───────────────────────▶│ compute missing (stat chunk files)        │
 │ parallel PUT missing chunks                 │  PUT  …/chunks/{id}     │ hash body → tmp → ledger → atomic rename  │
 │ commit → journal ack                        │  POST …/commit          │ COMMIT BARRIER → sync → versions (1 txn)  │
 │ list / restore / verify                     │  GET  /v1/chunks/{id}   │ verify (read-only)                        │
 └─────────────────────────────────────────────┘                         └─────────────────────────────────────────────┘
                                                                 vault/  chunks/ab/abcd…  tmp/  quarantine/  meta.db
```

---

## Main parts

- **Client (`bv`):** Scans the source directory, splits regular files into content-defined chunks (FastCDC v2020), hashes them with SHA-256, and builds a canonical sorted manifest. It opens an upload session (`POST /v1/uploads`), receives the list of missing chunk IDs, uploads only those missing chunks (`PUT /v1/uploads/{id}/chunks/{chunk_id}`), and issues a commit request (`POST /v1/uploads/{id}/commit`). For restore, it downloads chunks, re-verifies hashes, recreates directory trees, and applies file and directory timestamps.
- **Server (`bvd`):** Axum HTTP daemon (`127.0.0.1:7878`, configurable via `BV_LISTEN` and `BV_DATA_DIR`). Manages upload sessions in SQLite (`meta.db`), checks chunk existence directly against the filesystem (`vault/chunks/`), accepts new chunk payloads via temporary files (`vault/tmp/`), re-hashes them before atomic promotion, and enforces the zero-trust Commit Barrier before publishing versions.
- **Storage Layout:**
  - `vault/chunks/ab/abcdef01...`: Content-addressed chunk store with a 2-character hex directory fanout.
  - `vault/tmp/`: Staging directory for in-flight uploads, purged automatically on server startup.
  - `vault/quarantine/`: Isolated storage for chunks detected as corrupted or tampered during commit checks.
  - `vault/meta.db`: SQLite database in WAL mode (`synchronous = FULL`) tracking sessions, manifests, and versions.

---

## File list and chunks

- **Version representation:** A saved version records version ID (`v1`, `v2`), upload session ID, timestamp, byte totals (`total_bytes`, `uploaded_bytes`, `reused_bytes`), and canonical manifest. Entries are strictly sorted UTF-8 relative paths:
  - `dir`: path and `(mtime_secs, mtime_nanos)`.
  - `file`: path, logical size, `(mtime_secs, mtime_nanos)`, and an ordered list of `(chunk_id, length)` pairs. Empty files have an empty chunk list `[]`.
- **Chunking algorithm:** Regular files are chunked via FastCDC v2020 (`StreamCDC`) with 16 KiB min, 64 KiB avg, and 256 KiB max. Chunk boundaries resynchronise shortly after an edit, preserving deduplication across versions. Empty files produce 0 chunks.
- **Chunk IDs:** Lowercase hexadecimal SHA-256 digest of uncompressed chunk bytes (`sha256(chunk_bytes)`).
- **Byte accounting:**
  - `uploaded_bytes` is the sum of chunk lengths recorded in the `accepted` ledger—representing only chunks that *this upload session caused to be newly stored*. Repeated PUTs of already-stored chunks return HTTP 200 without writing to `accepted`, preventing double-counting.
  - `reused_bytes` is computed as `total_bytes - uploaded_bytes`.

---

## Safe completion and continuation

- **Structural invisibility:** Unfinished uploads reside strictly in the `uploads` table with `state = 'open'`. The `versions` table contains completed versions only. The default `bv list` command queries only `versions`, making unfinished uploads invisible by construction.
- **Commit Barrier and fsync:** Publication occurs only after re-verifying presence, size, and SHA-256 hash for every chunk in the manifest. All chunks are fsynced to disk, and the version row is inserted in a single atomic SQLite transaction (`BEGIN IMMEDIATE`), ensuring completed versions survive power loss.
- **Idempotent resume:** Sessions are keyed by `manifest_id = SHA-256(canonical_manifest)`. If an interrupted backup is re-run, `POST /v1/uploads` matches the existing open upload, queries on-disk chunks, and returns only the remaining `missing` chunks.
- **Unchanged folder backups:** Backing up an unchanged directory contacts the server, opens a session, detects all chunks exist (`missing: []`), and commits a new version with `uploaded_bytes: 0` and `reused_bytes: total_bytes`.

---

## Restore and verification

- **Exact restore:** Destination directory must be non-existent or empty. Directories are created first; chunks are downloaded, SHA-256 verified, and written to exact offsets; file mtimes are applied; directory mtimes are applied bottom-up. On any error, all created items are rolled back.
- **Read-only verification (`POST /v1/verify`):** Re-hashes all referenced chunks against their filenames and manifest specifications. Any discrepancy is reported with damage type (`HashMismatch`, `Missing`, `SizeMismatch`), affected version IDs, file paths, and chunk byte ranges (`bytes start..end`). The vault is never modified.

---

## Important choices and limits

- **FastCDC with 64 KiB average:** Re-synchronises boundaries after insertions, avoiding fixed-block cascade invalidation.
- **Zero-trust Commit Barrier:** Re-reading and hashing all chunks at commit adds minimal overhead while guaranteeing zero corrupt versions.
- **WAL mode with synchronous = FULL:** Guarantees database durability and crash safety.
- **Known limits:**
  - *No encryption or compression:* Omitted because they were not required by the brief and would complicate deterministic chunk deduplication and inspection.
  - *No authentication or access control:* Server binds to `127.0.0.1` for local single-tenant operation.
  - *Static source during backup:* Files must not be modified during client scanning.
  - *Special files skipped:* Symlinks, sockets, FIFOs, and device nodes are skipped with warnings.
  - *Case collisions:* Case-colliding names on case-insensitive filesystems are rejected at restore to prevent data loss.

---

## Appendix: Failure-Mode Recovery Matrix

| Failure Point | State Left Behind | Recovery Action |
|---|---|---|
| Client killed during scan | Nothing on server | Re-running restarts scan and resumes |
| Client killed between chunk uploads | Partial chunks stored, upload `open` | Re-run matches `manifest_id`, server returns remaining `missing`, only missing chunks sent |
| Client killed mid-PUT | Truncated file in `vault/tmp/` | Server discards tmp file; nothing promoted; chunk re-sent on resume |
| Server killed mid-write | Unreferenced file in `vault/tmp/` | Server startup wipes `vault/tmp/`; chunk never counted as present |
| Server killed after last chunk, before commit | All chunks stored, upload `open` | Client re-runs commit; 0 missing chunks; commit succeeds |
| Server killed during commit barrier | No `versions` row, or complete row (SQLite atomic txn) | Re-commit succeeds idempotently |
| Commit response lost over network | Version row exists on server, client unacknowledged | Client checks `GET /v1/uploads/{id}`; server returns existing version; no duplicate created |
| Stored chunk altered while upload is open | Chunk hash on disk differs from chunk ID | Commit barrier detects corruption, quarantines bad chunk, returns 409; client re-sends healthy chunk |
| Verification run on damaged vault | Read-only scan | Identifies damaged chunks and all dependent versions/files without modifying the vault |
