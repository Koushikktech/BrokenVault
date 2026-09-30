# BrokenVault - Klytos

Back up a folder, send only the data the server does not already have, survive interruptions, and restore every finished version exactly.

## Judge quick-start (about 90 seconds)

```bash
docker compose run --rm demo
```

This runs the whole judged scenario with real separate `bv` (client) and `bvd` (server) processes: back up V1, back up V2 (reused vs. uploaded bytes), kill client and server mid-upload, restart and resume, restore and compare every version, then damage a stored chunk and run `verify`. It ends with a PASS/FAIL scorecard. A saved run is in `docs/demo-output.txt`.
The first run builds the image (~2 minutes). No Docker? Use the native lane below.

**Design in five lines:** content-defined chunks (FastCDC, 64 KiB average) named by SHA-256 · unfinished uploads live in a separate table, so they cannot appear as versions · a commit step re-hashes every chunk before a version becomes visible · uploads resume by manifest hash, and repeated requests are safe · `verify` re-hashes all stored chunks and reports every affected version, file and byte range without changing anything. More detail: `docs/architecture.md`.

## Team

- Member 1: Krishna Koushik Kasarla
- Member 2: Siddartha Kailasa
- Member 3: Abhiram Nellutla

## Supported setup

- Operating system or Docker version: Ubuntu 22.04 and macOS (Apple Silicon / Intel), and Docker Engine 24+ with Compose v2.
- Programming language and version: Rust 1.85+ (edition 2024)
- Required tools: either Docker, or the Rust toolchain (`rustup`). `make` is not needed.

## Install

**Docker lane**

```bash
docker compose build
```

Wherever this README says `bv`, Docker users type `./dbv` (same arguments).

**Native lane**

```bash
cargo build --release --locked
export PATH="$PWD/target/release:$PATH"
```

The first build compiles bundled SQLite and takes ~1-2 minutes.

## Start the complete system

Terminal A (leave it running):

```bash
bvd                            # Docker lane: docker compose up --build
```

The server listens on `127.0.0.1:7878` and keeps its data in `./vault` (override with `BV_DATA_DIR=/path`). All other commands run in Terminal B.

## Commands

### Back up a folder

```bash
bv backup <FOLDER>
```

Prints the version ID, state, total bytes, uploaded bytes and reused bytes. Running the same command after an interruption continues the same upload. Backing up an unchanged directory records a new version with 0 uploaded bytes and 100% chunk reuse. Live transfer progress is shown on interactive terminals.

### List completed versions

```bash
bv list            # completed versions only
bv list --all      # also shows unfinished uploads
```

### Restore a version

```bash
bv restore <VERSION_ID> <EMPTY_DESTINATION_FOLDER>
```

The destination must not exist or must be empty. On any integrity failure the restore stops, removes what it created and names the damaged chunk, file and version.

### Verify stored data

```bash
bv verify                      # via the running server
bvd verify --data ./vault      # offline, no server needed
```

Read-only. Exit code 0 = healthy, 1 = damage found.

### Optional extras (bonus developer tooling)

- **Compare versions (diff):**
  ```bash
  bv diff <V1> <V2>                 # online via server
  bvd diff <V1> <V2> --data ./vault # offline direct read
  ```
  Shows file-level changes (`Added`, `Removed`, `Modified`) and chunk reuse statistics.
- **Vault storage analytics:**
  ```bash
  bv stats                          # online via server
  bvd stats --data ./vault          # offline direct read
  ```
  Reports unique physical chunks, logical vs. stored bytes, deduplication ratio, and space savings.
- **Partial path restore:**
  ```bash
  bv restore <VERSION_ID> <DESTINATION_FOLDER> --path <SUBPATH>
  ```
  Selectively restores a single file or subfolder hierarchy with strict path traversal validation.

## Run tests

```bash
cargo test --locked            # Docker lane: docker compose run --rm test
```

## Demo steps

The one-command version is `docker compose run --rm demo` (native: `./scripts/demo.sh`). To follow it by hand, unzip the organizers' `brokenvault_sample_v1.zip` and `brokenvault_sample_v2.zip` into `./sample_v1` and `./sample_v2`, start `bvd` in Terminal A, and run the rest in Terminal B.

1. **Back up version 1.**
   ```bash
   bv backup ./sample_v1
   bv list
   ```
2. **Back up version 2 and show reused and uploaded bytes.**
   ```bash
   bv backup ./sample_v2
   ```
   Expect uploaded bytes to be far below the folder's total size.
3. **Interrupt another upload and restart both programs.**
   ```bash
   cp -r ./sample_v2 ./sample_v3 && head -c 67108864 /dev/urandom > ./sample_v3/random.bin
   bv backup ./sample_v3 --stop-after-chunks 40    # simulates a crash after 40 chunks
   # Terminal A: stop bvd with Ctrl+C, then start it again with `bvd`
   bv list            # only v1 and v2: the unfinished upload is hidden
   bv list --all      # shows it as UNFINISHED
   ```
4. **Continue, complete and restore it.**
   ```bash
   bv backup ./sample_v3                            # resumes; sends only missing chunks
   bv restore v1 ./restored_v1
   bv restore v2 ./restored_v2
   bv restore v3 ./restored_v3
   bv diff v1 v2                                    # compare version manifests
   bv stats                                         # view vault deduplication savings
   bv dev diff ./sample_v1 ./restored_v1            # bytes, empty dirs, mtimes
   ```
5. **Change or remove one stored chunk and run verification.**
   ```bash
   bvd debug damage --data ./vault --mode flip      # flips a byte in one stored chunk
   bv verify                                        # names each affected version, file path and byte range
   ```

## Known limits

- No login or access control (out of scope); the server binds to loopback by default.
- No encryption or compression (not required by the brief).
- Source folders must not change while a backup runs.
- Symlinks, FIFOs and device files are skipped with a warning.
- On case-insensitive file systems, names that differ only by letter case collide at restore time.
- The commit step re-hashes every chunk of the version, which adds a few seconds per GB.
- No garbage collection or deletion of versions.

## External and AI-assisted work

- Libraries: `axum` 0.8 and `tokio` (HTTP server), `fastcdc` 3.x (chunking), `rusqlite` 0.40 with bundled SQLite (metadata), `sha2` (SHA-256), `rayon` (parallelism), `ureq` 3 (HTTP client), `walkdir` and `filetime` (scanning, mtimes), `clap` (CLI), `serde` / `serde_json` (manifests), `ctrlc` (clean pause). No external services.
- AI tools used: Antigravity (Gemini) for scaffolding protocol structs and error types, property tests, and the Dockerfile and demo script; Claude for design planning and README/documentation review.
- The team ran and reviewed everything. The tests and the demo scorecard are the evidence for the claims above.
