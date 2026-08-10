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
  fail "usage: build-daemon-archive.sh VERSION [OUTPUT_DIRECTORY]"
fi
if [[ "$(uname -s)" != "Linux" || "$(uname -m)" != "x86_64" ]]; then
  fail "the Public Alpha daemon archive currently requires Linux x86-64"
fi

readonly release_version="$1"
readonly requested_output_directory="${2:-${repository_root}/dist}"
"${script_directory}/verify-version.sh" taskcaged "${release_version}" >/dev/null

for required_command in cargo install mktemp readlink sha256sum tar; do
  command -v "${required_command}" >/dev/null 2>&1 || fail "required command is missing: ${required_command}"
done

mkdir -p -- "${requested_output_directory}"
output_directory="$(cd -- "${requested_output_directory}" && pwd)"
readonly output_directory
readonly archive_root="taskcage-v${release_version}-x86_64-unknown-linux-gnu"
readonly archive_path="${output_directory}/${archive_root}.tar.gz"
readonly checksum_path="${archive_path}.sha256"

[[ ! -e "${archive_path}" ]] || fail "release archive already exists: ${archive_path}"
[[ ! -e "${checksum_path}" ]] || fail "release checksum already exists: ${checksum_path}"

staging_directory="$(mktemp -d /tmp/taskcage-release.XXXXXX)"
resolved_staging_directory="$(readlink -f "${staging_directory}")"
cleanup() {
  if [[ -n "${resolved_staging_directory}" && -d "${resolved_staging_directory}" ]]; then
    case "${resolved_staging_directory}" in
      /tmp/taskcage-release.*) rm -rf -- "${resolved_staging_directory}" ;;
      *) echo "WARN: refusing to remove unexpected staging path: ${resolved_staging_directory}" >&2 ;;
    esac
  fi
}
trap cleanup EXIT

cd "${repository_root}"
cargo build --locked --release --package taskcaged

install -D -m 0755 target/release/taskcaged "${staging_directory}/${archive_root}/bin/taskcaged"
install -D -m 0755 packaging/ubuntu/install-taskcaged.sh \
  "${staging_directory}/${archive_root}/packaging/ubuntu/install-taskcaged.sh"
install -D -m 0755 packaging/ubuntu/uninstall-taskcaged.sh \
  "${staging_directory}/${archive_root}/packaging/ubuntu/uninstall-taskcaged.sh"
install -D -m 0644 packaging/ubuntu/taskcaged.env \
  "${staging_directory}/${archive_root}/packaging/ubuntu/taskcaged.env"
install -D -m 0644 packaging/ubuntu/taskcaged.service \
  "${staging_directory}/${archive_root}/packaging/ubuntu/taskcaged.service"
install -D -m 0644 README.md "${staging_directory}/${archive_root}/README.md"
install -D -m 0644 LICENSE "${staging_directory}/${archive_root}/LICENSE"

source_date_epoch="${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct)}"
[[ "${source_date_epoch}" =~ ^[0-9]+$ ]] || fail "SOURCE_DATE_EPOCH must be an integer"
tar \
  --sort=name \
  --mtime="@${source_date_epoch}" \
  --owner=0 \
  --group=0 \
  --numeric-owner \
  -C "${staging_directory}" \
  -czf "${archive_path}" \
  "${archive_root}"

(
  cd "${output_directory}"
  sha256sum "$(basename -- "${archive_path}")" >"$(basename -- "${checksum_path}")"
  sha256sum --check "$(basename -- "${checksum_path}")"
)

echo "archive=${archive_path}"
echo "checksum=${checksum_path}"
