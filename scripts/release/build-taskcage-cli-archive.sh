#!/usr/bin/env bash
set -euo pipefail

script_directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly script_directory
repository_root="$(cd -- "${script_directory}/../.." && pwd)"
readonly repository_root

fail() {
  echo "ERROR: $*" >&2
  exit 1
}

if [[ $# -lt 1 || $# -gt 2 ]]; then
  fail "usage: build-taskcage-cli-archive.sh VERSION [OUTPUT_DIRECTORY]"
fi

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) release_target="x86_64-unknown-linux-gnu" ;;
  Linux-aarch64|Linux-arm64) release_target="aarch64-unknown-linux-gnu" ;;
  Darwin-x86_64) release_target="x86_64-apple-darwin" ;;
  Darwin-arm64) release_target="aarch64-apple-darwin" ;;
  *) fail "unsupported taskcage CLI release platform: $(uname -s) $(uname -m)" ;;
esac
readonly release_target

readonly release_version="$1"
readonly requested_output_directory="${2:-${repository_root}/dist}"
"${script_directory}/verify-version.sh" taskcage "${release_version}" >/dev/null

for required_command in cargo mktemp tar gzip cp chmod mkdir; do
  command -v "${required_command}" >/dev/null 2>&1 || fail "required command is missing: ${required_command}"
done

mkdir -p -- "${requested_output_directory}"
output_directory="$(cd -- "${requested_output_directory}" && pwd)"
readonly output_directory
readonly archive_root="taskcage-cli-v${release_version}-${release_target}"
readonly archive_path="${output_directory}/${archive_root}.tar.gz"
readonly checksum_path="${archive_path}.sha256"

[[ ! -e "${archive_path}" ]] || fail "release archive already exists: ${archive_path}"
[[ ! -e "${checksum_path}" ]] || fail "release checksum already exists: ${checksum_path}"

staging_directory="$(mktemp -d "${TMPDIR:-/tmp}/taskcage-cli-release.XXXXXX")"
cleanup() {
  case "${staging_directory}" in
    "${TMPDIR:-/tmp}"/taskcage-cli-release.*|/tmp/taskcage-cli-release.*) rm -rf -- "${staging_directory}" ;;
    *) echo "WARN: refusing to remove unexpected staging path: ${staging_directory}" >&2 ;;
  esac
}
trap cleanup EXIT

cd "${repository_root}"
cargo build --locked --release --package taskcaged --bin taskcage

mkdir -p "${staging_directory}/${archive_root}/bin"
cp target/release/taskcage "${staging_directory}/${archive_root}/bin/taskcage"
chmod 0755 "${staging_directory}/${archive_root}/bin/taskcage"
cp docs/install-taskcage-cli.md "${staging_directory}/${archive_root}/README.md"
cp docs/capsule-builder.md "${staging_directory}/${archive_root}/CAPSULEFILE.md"
cp LICENSE "${staging_directory}/${archive_root}/LICENSE"

(
  cd "${staging_directory}"
  COPYFILE_DISABLE=1 tar -cf - "${archive_root}" | gzip -n >"${archive_path}"
)

if command -v sha256sum >/dev/null 2>&1; then
  (
    cd "${output_directory}"
    sha256sum "$(basename -- "${archive_path}")" >"$(basename -- "${checksum_path}")"
    sha256sum --check "$(basename -- "${checksum_path}")"
  )
else
  (
    cd "${output_directory}"
    shasum -a 256 "$(basename -- "${archive_path}")" >"$(basename -- "${checksum_path}")"
    shasum -a 256 --check "$(basename -- "${checksum_path}")"
  )
fi

echo "archive=${archive_path}"
echo "checksum=${checksum_path}"
