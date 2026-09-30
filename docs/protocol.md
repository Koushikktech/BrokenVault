# BrokenVault HTTP Protocol Specification (v1)

## Overview

BrokenVault communicates over HTTP/1.1 with JSON payloads for metadata and raw byte streams for chunk storage and retrieval.

- Default Server Port: `7878`
- Default Base URL: `http://127.0.0.1:7878`
- Manifest Ingestion Body Limit: `64 MiB`

---

## Canonical Manifest Schema

Manifests are serialized as deterministic canonical JSON. Entries are sorted strictly by relative path in ascending UTF-8 byte order.

```json
{
  "format": "brokenvault-manifest/1",
  "chunker": {
    "algo": "fastcdc-v2020",
    "min": 16384,
    "avg": 65536,
    "max": 262144
  },
  "entries": [
    {
      "type": "dir",
      "path": "docs",
      "mtime": [1700000000, 0]
    },
    {
      "type": "file",
      "path": "docs/readme.txt",
      "size": 100,
      "mtime": [1700000000, 0],
      "chunks": [
        ["0def9353b67db439fe14e2fb1f99b3363c2bd16197631604a5170782e0683ed3", 100]
      ]
    }
  ]
}
```

- `chunks`: Ordered list of `[chunk_id, length]` pairs. File offsets are derived as the running sum of preceding chunk lengths.
- `manifest_id`: Lowercase hexadecimal SHA-256 hash of the canonical manifest JSON bytes (64 hex characters).

---

## Error Model & Status Codes

All non-2xx responses return a standardized JSON error object:

```json
{
  "code": "HASH_MISMATCH",
  "message": "computed hash does not match requested chunk id",
  "hint": "re-upload the chunk with correct content"
}
```

### Error Code Reference

| HTTP Status | Error Code | Description |
|---|---|---|
| 400 Bad Request | `UPLOAD_NOT_OPEN` | Upload session is already committed or aborted |
| 400 Bad Request | `NOT_IN_MANIFEST` | Chunk ID is not referenced by the upload's manifest |
| 400 Bad Request | `INVALID_PATH` | Path contains `..`, null bytes, or illegal characters |
| 404 Not Found | `UPLOAD_NOT_FOUND` | Specified upload ID does not exist |
| 404 Not Found | `VERSION_NOT_FOUND` | Specified version ID does not exist |
| 404 Not Found | `CHUNK_NOT_FOUND` | Requested chunk does not exist on disk |
| 409 Conflict | `COMMIT_CONFLICT` | One or more chunks are missing or corrupt during commit barrier |
| 410 Gone | `NOT_FOUND_OR_CLOSED` | Upload session was aborted or already closed |
| 422 Unprocessable Entity | `HASH_MISMATCH` | Computed SHA-256 of payload does not match chunk ID |
| 422 Unprocessable Entity | `SIZE_MISMATCH` | Payload byte length does not match manifest chunk length |
| 500 Internal Error | `CHUNK_CORRUPT` | Stored chunk failed SHA-256 re-check during download |

---

## API Endpoints

### 1. Health & Status
- **`GET /v1/health`**
  - **Response 200 OK**:
    ```json
    {
      "vault_id": "vlt_9c1f3e5a7b204812",
      "protocol_version": "1.0",
      "status": "ok"
    }
    ```

### 2. Upload Sessions
- **`POST /v1/uploads`**
  - **Body**: Canonical JSON manifest bytes.
  - **Behavior**:
    - If an **open** upload with matching `manifest_id` exists, resumes that session (`resumed: true`).
    - If the manifest was previously committed, creates a new session (`resumed: false`). Because all chunks already exist in storage, `missing` is empty (`[]`). Committing this session records a new version with `uploaded_bytes: 0` and `reused_bytes: total_bytes`.
    - Otherwise, creates a new open upload session and queries on-disk chunks to return the list of missing chunk IDs.
  - **Response 201 Created (new) / 200 OK (resumed)**:
    ```json
    {
      "upload_id": "upl_9c1f3e5a7b204812",
      "resumed": false,
      "total_bytes": 537001984,
      "chunks_total": 8216,
      "missing": [
        "9f2c8a1b3d5e7f092468ace013579bdf2468ace013579bdf2468ace013579bdf",
        "3a1b2c3d4e5f60718293a4b5c6d7e8f90123456789abcdef0123456789abcdef"
      ]
    }
    ```

- **`GET /v1/uploads`**
  - **Response 200 OK**: Array of active open uploads:
    ```json
    [
      {
        "upload_id": "upl_9c1f3e5a7b204812",
        "total_bytes": 537001984,
        "created_at": 1767225600,
        "state": "open",
        "chunks_total": 8216,
        "chunks_present": 8100
      }
    ]
    ```

- **`GET /v1/uploads/{upload_id}`**
  - **Response 200 OK**:
    ```json
    {
      "upload_id": "upl_9c1f3e5a7b204812",
      "state": "open",
      "total_bytes": 537001984,
      "chunks_total": 8216,
      "missing": [
        "9f2c8a1b3d5e7f092468ace013579bdf2468ace013579bdf2468ace013579bdf"
      ],
      "version_id": null
    }
    ```

- **`DELETE /v1/uploads/{upload_id}`**
  - **Response 204 No Content**: Marks session as aborted and removes staging files.

- **`PUT /v1/uploads/{upload_id}/chunks/{chunk_id}`**
  - **Body**: Raw chunk bytes.
  - **Behavior**:
    - Validates that chunk belongs to manifest and size matches.
    - If chunk already exists in storage, returns `200 OK` immediately without writing to the `accepted` ledger, preventing double-counting of uploaded bytes.
    - Otherwise verifies SHA-256 hash, writes to staging, atomically promotes to `vault/chunks/`, and records in `accepted` ledger.
  - **Response 201 Created (new chunk stored) / 200 OK (already present, no ledger insertion)**.

- **`POST /v1/uploads/{upload_id}/commit`**
  - **Behavior**: Zero-trust commit barrier. Parallel re-verification of all referenced chunks (presence, length, SHA-256). Fsyncs chunks and assigns next version ID in a single atomic SQLite transaction.
  - **Response 200 OK**:
    ```json
    {
      "version": "v1",
      "committed_at": 1767225642,
      "total_bytes": 537001984,
      "uploaded_bytes": 537001984,
      "reused_bytes": 0,
      "files": 132,
      "dirs": 18,
      "chunks": 8216
    }
    ```
  - **Response 409 Conflict**:
    ```json
    {
      "error": "cannot commit upload: missing or corrupt chunks",
      "missing_or_corrupt": ["9f2c...e1"]
    }
    ```

### 3. Versions
- **`GET /v1/versions`**
  - **Response 200 OK**: Array of completed versions (unfinished uploads are excluded by construction):
    ```json
    [
      {
        "id": "v1",
        "manifest_id": "9c1f3e5a7b2048123456789abcdef0123456789abcdef0123456789abcdef012",
        "committed_at": 1767225642,
        "total_bytes": 537001984,
        "uploaded_bytes": 537001984,
        "reused_bytes": 0,
        "files": 132,
        "dirs": 18,
        "chunks": 8216
      }
    ]
    ```

- **`GET /v1/versions/{version_id}`**
  - **Response 200 OK**: Single `VersionSummary` object as above.

- **`GET /v1/versions/{version_id}/manifest`**
  - **Response 200 OK**: Canonical manifest JSON bytes for the version.

### 4. Chunk Retrieval
- **`GET /v1/chunks/{chunk_id}`**
  - **Behavior**: Streams chunk bytes. Re-computes SHA-256 before transmission. Returns 500 `CHUNK_CORRUPT` upon bit-rot.
  - **Response 200 OK**: `application/octet-stream` chunk bytes.

### 5. Integrity Verification
- **`POST /v1/verify`**
  - **Behavior**: Read-only verification of all chunks across all completed versions.
  - **Response 200 OK**:
    ```json
    {
      "healthy": false,
      "damaged_chunks": [
        {
          "chunk_id": "9f2c8a1b3d5e7f092468ace013579bdf2468ace013579bdf2468ace013579bdf",
          "damage_type": "HashMismatch",
          "expected_len": 65536,
          "actual_len": 65536,
          "affected": [
            {
              "version_id": "v1",
              "path": "video/intro.mp4",
              "chunk_index": 212,
              "start_byte": 13893632,
              "end_byte": 13959167
            }
          ]
        }
      ],
      "healthy_versions": [],
      "info_messages": []
    }
    ```

### 6. Vault Analytics
- **`GET /v1/stats`**
  - **Behavior**: Computes global storage analytics and deduplication metrics.
  - **Response 200 OK**:
    ```json
    {
      "vault_id": "vlt_9c1f3e5a7b204812",
      "completed_versions": 3,
      "open_uploads": 0,
      "total_logical_bytes": 1611005952,
      "total_physical_chunk_bytes": 553018240,
      "unique_chunks": 8450,
      "deduplication_ratio": 2.91,
      "space_saved_bytes": 1057987712,
      "space_saved_percent": 65.67
    }
    ```

---

## Client-Side Operations

### Version Diff Computation
Version-to-version diffs (`bv diff <v1> <v2>`) are computed client-side by retrieving the canonical manifests from `GET /v1/versions/{v1}/manifest` and `GET /v1/versions/{v2}/manifest`. The client evaluates added, removed, and modified files, along with chunk reuse statistics.
