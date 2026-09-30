#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${ROOT_DIR}"
command -v python3 >/dev/null || { echo "python3 is required" >&2; exit 1; }
command -v curl >/dev/null || { echo "curl is required" >&2; exit 1; }

if command -v cargo >/dev/null; then
    cargo build --release --locked --bin bv --bin bvd
fi
BV="${ROOT_DIR}/target/release/bv"
BVD="${ROOT_DIR}/target/release/bvd"
if [[ ! -x "$BV" ]]; then BV=$(command -v bv) || fail_missing="bv"; fi
if [[ ! -x "$BVD" ]]; then BVD=$(command -v bvd) || fail_missing="bvd"; fi
[[ -z "${fail_missing:-}" ]] || { echo "${fail_missing} is required" >&2; exit 1; }
WORK_DIR=$(mktemp -d "${TMPDIR:-/tmp}/brokenvault_demo_XXXXXX")
BVD_PID=""
kill_server() {
    if [[ -n "${BVD_PID}" ]] && kill -0 "${BVD_PID}" 2>/dev/null; then
        kill -9 "${BVD_PID}" 2>/dev/null || true
        wait "${BVD_PID}" 2>/dev/null || true
    fi
    BVD_PID=""
}
trap 'kill_server; rm -rf "${WORK_DIR}"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }
require() { "$@" || fail "assertion failed: $*"; }

VAULT_DIR="${WORK_DIR}/vault"
export BV_STATE_DIR="${WORK_DIR}/state"
# Use a free port instead of the default, to avoid attaching to an unrelated vault.
PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')
export BV_SERVER="http://127.0.0.1:${PORT}"
start_server() {
    "${BVD}" serve --data "${VAULT_DIR}" --listen "127.0.0.1:${PORT}" > "${WORK_DIR}/server.log" 2>&1 &
    BVD_PID=$!
    for _ in {1..100}; do
        if ! kill -0 "${BVD_PID}" 2>/dev/null; then break; fi
        if curl --fail --silent "${BV_SERVER}/v1/health" > /dev/null; then return; fi
        sleep 0.1
    done
    cat "${WORK_DIR}/server.log" >&2
    fail "server did not become healthy"
}
# Check the JSON shape and membership, not merely a substring in formatted output.
check_list() {
    local expected="$1" open_id="$2" partial="$3"
    "${BV}" list --json | python3 -c '
import json,sys
x=json.load(sys.stdin)
assert [v["id"] for v in x["versions"]] == (sys.argv[1].split(",") if sys.argv[1] else []), x
assert x["open_uploads"] == [], x
' "$expected"
    "${BV}" list --all --json | python3 -c '
import json, sys
wanted, upload_id, partial = sys.argv[1:]
data = json.load(sys.stdin)
versions = data["versions"]
assert [v["id"] for v in versions] == (wanted.split(",") if wanted else []), versions
uploads = data["open_uploads"]
assert len(uploads) == (1 if upload_id else 0), uploads
if upload_id:
    up = uploads[0]
    assert up["upload_id"] == upload_id and up["state"] == "open", up
    if partial == "yes":
        assert 0 < up["chunks_present"] < up["chunks_total"], up
print("list: versions=%s open=%s" % (wanted or "none", upload_id or "none"))
' "$expected" "$open_id" "$partial"
}
open_id() {
    "${BV}" list --all --json | python3 -c 'import json,sys; x=json.load(sys.stdin); assert len(x["open_uploads"])==1, x; print(x["open_uploads"][0]["upload_id"])'
}
version_check() {
    local version="$1" upload_id="$2"
    "${BV}" list --json | python3 -c '
import json,sys
version, upload_id = sys.argv[1:]
v = next(v for v in json.load(sys.stdin)["versions"] if v["id"] == version)
assert v["upload_id"] == upload_id and v["total_bytes"] > 0, v
assert v["uploaded_bytes"] > 0 and v["uploaded_bytes"] <= v["total_bytes"], v
print("%s: upload=%s uploaded=%s reused=%s total=%s" % (version, upload_id, v["uploaded_bytes"], v["reused_bytes"], v["total_bytes"]))
' "$version" "$upload_id"
}
snapshot() {
    python3 -c '
import hashlib, os, sys
root = sys.argv[1]
for base, dirs, files in os.walk(root):
    for name in sorted(files):
        if name.endswith("-shm"): continue  # SQLite shared-memory locks are ephemeral.
        path = os.path.join(base, name)
        st = os.stat(path)
        with open(path, "rb") as f: digest = hashlib.sha256(f.read()).hexdigest()
        print(os.path.relpath(path, root), st.st_size, st.st_mtime_ns, digest)
' "$VAULT_DIR"
}

SRC_V1="${WORK_DIR}/sample_v1"
SRC_V2="${WORK_DIR}/sample_v2"
SRC_V3="${WORK_DIR}/sample_v3"
echo "BrokenVault interruption / continuation proof (${WORK_DIR})"
"${BV}" dev gen "${SRC_V1}" --seed 101
start_server
check_list "" "" no

echo "[1] Pause V1 after three accepted chunks (controlled interruption, exit 130)."
set +e
"${BV}" backup "${SRC_V1}" --jobs 1 --stop-after-chunks 3
stopped=$?
set -e
[[ "$stopped" -eq 130 ]] || fail "V1 pause returned $stopped, expected 130"
V1_ID=$(open_id)
check_list "" "$V1_ID" yes
kill_server  # SIGKILL the daemon with an unfinished upload in its persistent vault.
start_server
check_list "" "$V1_ID" yes
resume_output=$("${BV}" backup "${SRC_V1}" --jobs 1) || fail "V1 resume failed"
echo "$resume_output"
[[ "$resume_output" == *"Upload ${V1_ID} (resumed)"* ]] || fail "V1 did not resume the same upload"
check_list "v1" "" no
version_check v1 "$V1_ID"

# Preserve mtimes when copying: the manifest includes mtimes.
cp -pR "${SRC_V1}" "${SRC_V2}"
"${BV}" dev gen "${SRC_V2}" --seed 101 --mutate
"${BV}" backup "${SRC_V2}"
check_list "v1,v2" "" no
"${BV}" list --json | python3 -c '
import json,sys
v = next(v for v in json.load(sys.stdin)["versions"] if v["id"] == "v2")
assert 0 < v["uploaded_bytes"] < v["total_bytes"] and v["reused_bytes"] > 0, v
print("v2 dedup: uploaded=%s reused=%s" % (v["uploaded_bytes"], v["reused_bytes"]))
'
cp -pR "${SRC_V2}" "${SRC_V3}"
printf 'V3 has unique bytes not present in V2.\n' > "${SRC_V3}/version3.txt"
python3 -c 'import os,sys; open(sys.argv[1], "wb").write(os.urandom(4 * 1024 * 1024))' "${SRC_V3}/v3_unique.bin"
[[ ! -e "${SRC_V2}/v3_unique.bin" ]] || fail "V3 must differ from V2"

echo "[2] Pause distinct V3, restart daemon, then resume the same upload."
set +e
"${BV}" backup "${SRC_V3}" --jobs 1 --stop-after-chunks 1
stopped=$?
set -e
[[ "$stopped" -eq 130 ]] || fail "V3 pause returned $stopped, expected 130"
V3_ID=$(open_id)
check_list "v1,v2" "$V3_ID" yes
kill_server
start_server
check_list "v1,v2" "$V3_ID" yes
resume_output=$("${BV}" backup "${SRC_V3}" --jobs 1) || fail "V3 resume failed"
echo "$resume_output"
[[ "$resume_output" == *"Upload ${V3_ID} (resumed)"* ]] || fail "V3 did not resume the same upload"
check_list "v1,v2,v3" "" no
version_check v3 "$V3_ID"

for n in 1 2 3; do
    src="${WORK_DIR}/sample_v${n}"
    dest="${WORK_DIR}/restore_v${n}"
    "${BV}" restore "v${n}" "$dest"
    "${BV}" dev diff "$src" "$dest" || fail "v${n} restore differs"
done

echo "[3] Flip a stored chunk, verify exact reported impact and read-only behavior."
damage_output=$("${BVD}" debug damage --data "$VAULT_DIR" --mode flip)
echo "$damage_output"
[[ "$damage_output" =~ ^Damaged\ chunk\ ([0-9a-f]{64})\ using\ mode\ Flip$ ]] || fail "damage command did not name the damaged chunk"
damaged_chunk="${BASH_REMATCH[1]}"
snapshot > "${WORK_DIR}/before.verify"
set +e
"${BV}" verify --json > "${WORK_DIR}/report.json"
verify_exit=$?
set -e
cat "${WORK_DIR}/report.json"
[[ "$verify_exit" -eq 1 ]] || fail "damaged verify returned $verify_exit, expected 1"
affected=$(python3 -c '
import json,sys
report=json.load(open(sys.argv[1]))
assert report["healthy"] is False and len(report["damaged_chunks"]) == 1, report
assert report["damaged_chunks"][0]["chunk_id"] == sys.argv[2], report
for chunk in report["damaged_chunks"]:
    assert chunk["affected"], chunk
    for ref in chunk["affected"]:
        assert ref["version_id"] in ("v1","v2","v3") and ref["path"] and ref["start_byte"] <= ref["end_byte"], ref
print(report["damaged_chunks"][0]["affected"][0]["version_id"])
' "${WORK_DIR}/report.json" "$damaged_chunk") || fail "verify did not report version/file/byte impact"
snapshot > "${WORK_DIR}/after.verify"
cmp "${WORK_DIR}/before.verify" "${WORK_DIR}/after.verify" || fail "verify modified persistent vault files"
set +e
"${BV}" restore "$affected" "${WORK_DIR}/restore_damaged" > "${WORK_DIR}/restore_error.log" 2>&1
restore_exit=$?
cat "${WORK_DIR}/restore_error.log"
set -e
[[ "$restore_exit" -eq 1 ]] || fail "restore of damaged $affected returned $restore_exit, expected 1"
grep -Fq "$damaged_chunk" "${WORK_DIR}/restore_error.log" || fail "restore failed for a reason other than the damaged chunk"
if [[ -e "${WORK_DIR}/restore_damaged" ]]; then
    [[ -d "${WORK_DIR}/restore_damaged" ]] || fail "failed restore left an unexpected file"
    [[ -z "$(ls -A "${WORK_DIR}/restore_damaged")" ]] || fail "failed restore left partial files"
fi
echo "PASS: unfinished uploads hidden; same IDs resumed; V1/V2/V3 exact; damaged $affected restore rejected; verify read-only."
