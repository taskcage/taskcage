#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "ERROR: usage: taskcage-cli-artifact-smoke.sh VERSION ARCHIVE CHECKSUM" >&2
  exit 1
fi

for required_command in python3 tar mktemp; do
  command -v "${required_command}" >/dev/null 2>&1 || {
    echo "ERROR: required command is missing: ${required_command}" >&2
    exit 1
  }
done

readonly release_version="$1"
archive_path="$(cd -- "$(dirname -- "$2")" && pwd)/$(basename -- "$2")"
readonly archive_path
checksum_path="$(cd -- "$(dirname -- "$3")" && pwd)/$(basename -- "$3")"
readonly checksum_path

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) release_target="x86_64-unknown-linux-gnu" ;;
  Linux-aarch64|Linux-arm64) release_target="aarch64-unknown-linux-gnu" ;;
  Darwin-x86_64) release_target="x86_64-apple-darwin" ;;
  Darwin-arm64) release_target="aarch64-apple-darwin" ;;
  *) echo "SKIP: unsupported CLI smoke platform" >&2; exit 77 ;;
esac
readonly release_target
readonly archive_root="taskcage-cli-v${release_version}-${release_target}"

[[ -f "${archive_path}" && ! -L "${archive_path}" ]] || {
  echo "ERROR: CLI archive must be a regular file" >&2
  exit 1
}
[[ -f "${checksum_path}" && ! -L "${checksum_path}" ]] || {
  echo "ERROR: CLI checksum must be a regular file" >&2
  exit 1
}

if command -v sha256sum >/dev/null 2>&1; then
  (
    cd "$(dirname -- "${archive_path}")"
    sha256sum --check "$(basename -- "${checksum_path}")"
  )
else
  (
    cd "$(dirname -- "${archive_path}")"
    shasum -a 256 --check "$(basename -- "${checksum_path}")"
  )
fi

python3 - "${archive_path}" "${archive_root}" <<'PY'
import sys
import tarfile

archive_path, root = sys.argv[1:]
required_files = {
    f"{root}/bin/taskcage",
    f"{root}/README.md",
    f"{root}/CAPSULEFILE.md",
    f"{root}/LICENSE",
}
allowed_directories = {root, f"{root}/bin"}
seen = set()
with tarfile.open(archive_path, "r:gz") as archive:
    for member in archive.getmembers():
        name = member.name.rstrip("/")
        if not name or name.startswith("/") or ".." in name.split("/"):
            raise SystemExit(f"ERROR: unsafe archive entry: {member.name}")
        if name in seen:
            raise SystemExit(f"ERROR: duplicate archive entry: {member.name}")
        seen.add(name)
        if member.isdir() and name in allowed_directories:
            continue
        if member.isfile() and name in required_files:
            continue
        raise SystemExit(f"ERROR: unexpected archive entry or type: {member.name}")
missing = required_files - seen
if missing:
    raise SystemExit(f"ERROR: archive is missing required files: {sorted(missing)}")
PY

test_root="$(mktemp -d "${TMPDIR:-/tmp}/taskcage-cli-smoke.XXXXXX")"
cleanup() {
  case "${test_root}" in
    "${TMPDIR:-/tmp}"/taskcage-cli-smoke.*|/tmp/taskcage-cli-smoke.*) rm -rf -- "${test_root}" ;;
    *) echo "WARN: refusing to remove unexpected test path: ${test_root}" >&2 ;;
  esac
}
trap cleanup EXIT

tar -xzf "${archive_path}" -C "${test_root}"
binary="${test_root}/${archive_root}/bin/taskcage"
[[ -f "${binary}" && ! -L "${binary}" && -x "${binary}" ]] || {
  echo "ERROR: taskcage binary is missing or unsafe" >&2
  exit 1
}

set +e
output="$("${binary}" capsule build 2>&1)"
status=$?
set -e
[[ ${status} -ne 0 ]]
grep -F 'capsule build file' <<<"${output}" >/dev/null

echo "PASS: v${release_version} taskcage CLI archive checksum, layout, and command dispatch verified"
