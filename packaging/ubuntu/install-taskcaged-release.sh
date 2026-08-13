#!/usr/bin/env bash
set -euo pipefail

readonly default_release_base_url="https://github.com/taskcage/taskcage/releases/download"
readonly release_api_url="${TASKCAGE_RELEASE_API_URL:-https://api.github.com/repos/taskcage/taskcage/releases?per_page=100}"

release_version=""
release_url=""
start_service=true
download_directory=""

usage() {
  cat <<'EOF'
Usage: install-taskcaged.sh [--version VERSION] [--no-autostart] [--release-url URL]

  --version VERSION  Install a specific taskcaged GitHub release (default: latest public release).
  --no-autostart     Install without enabling or starting taskcaged.service.
  --release-url URL  Override the version-specific release URL (for mirrors or tests).
EOF
}

fail() {
  echo "ERROR: $*" >&2
  exit 1
}

cleanup() {
  if [[ -n "${download_directory}" && -d "${download_directory}" ]]; then
    case "${download_directory}" in
      /tmp/taskcage-release-install.*) rm -rf -- "${download_directory}" ;;
      *) echo "WARN: refusing to remove unexpected download path: ${download_directory}" >&2 ;;
    esac
  fi
}
trap cleanup EXIT

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      [[ $# -ge 2 ]] || fail "--version requires a value"
      [[ -z "${release_version}" ]] || fail "--version may be specified only once"
      release_version="$2"
      shift 2
      ;;
    --no-autostart)
      start_service=false
      shift
      ;;
    --release-url)
      [[ $# -ge 2 ]] || fail "--release-url requires a value"
      [[ -z "${release_url}" ]] || fail "--release-url may be specified only once"
      release_url="${2%/}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      fail "unknown option: $1"
      ;;
  esac
done

[[ "${EUID}" -eq 0 ]] || fail "run this installer as root"
[[ "$(uname -s)" == "Linux" ]] || fail "taskcaged release installation requires Linux"
case "$(uname -m)" in
  x86_64) release_target="x86_64-unknown-linux-gnu" ;;
  aarch64) release_target="aarch64-unknown-linux-gnu" ;;
  *) fail "taskcaged releases support Linux x86_64 or aarch64" ;;
esac
readonly release_target

for required_command in curl mktemp python3 readlink sha256sum tar; do
  command -v "${required_command}" >/dev/null 2>&1 || fail "required command is missing: ${required_command}"
done
created_download_directory="$(mktemp -d /tmp/taskcage-release-install.XXXXXX)"
download_directory="$(readlink -f "${created_download_directory}")"

if [[ -z "${release_version}" ]]; then
  readonly release_metadata_path="${download_directory}/releases.json"
  curl --fail --location --silent --show-error \
    --header "Accept: application/vnd.github+json" \
    --header "X-GitHub-Api-Version: 2022-11-28" \
    --output "${release_metadata_path}" \
    "${release_api_url}"
  if ! release_version="$(python3 - "${release_metadata_path}" <<'PY'
import json
import re
import sys

with open(sys.argv[1], encoding="utf-8") as release_file:
    releases = json.load(release_file)

candidates = []
for release in releases:
    if release.get("draft"):
        continue
    match = re.fullmatch(r"taskcaged-v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", release.get("tag_name", ""))
    if match:
        version = tuple(int(part) for part in match.groups())
        candidates.append(version)

if not candidates:
    raise SystemExit("no public taskcaged release was found")

print(".".join(str(part) for part in max(candidates)))
PY
  )"; then
    fail "could not select the latest public taskcaged release"
  fi
  echo "Selected latest public taskcaged release: ${release_version}"
fi

[[ "${release_version}" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] || \
  fail "--version must be a Semantic Version such as 0.1.0"

if [[ -z "${release_url}" ]]; then
  release_url="${default_release_base_url}/taskcaged-v${release_version}"
fi

readonly archive_root="taskcage-v${release_version}-${release_target}"
readonly archive_name="${archive_root}.tar.gz"
readonly checksum_name="${archive_name}.sha256"

readonly archive_path="${download_directory}/${archive_name}"
readonly checksum_path="${download_directory}/${checksum_name}"
readonly verified_checksum_path="${download_directory}/verified.sha256"

curl --fail --location --silent --show-error \
  --output "${archive_path}" \
  "${release_url}/${archive_name}"
curl --fail --location --silent --show-error \
  --output "${checksum_path}" \
  "${release_url}/${checksum_name}"

mapfile -t checksum_lines <"${checksum_path}"
[[ "${#checksum_lines[@]}" -eq 1 ]] || fail "release checksum must contain exactly one entry"
read -r checksum_digest checksum_file checksum_extra <<<"${checksum_lines[0]}"
[[ "${checksum_digest}" =~ ^[0-9a-fA-F]{64}$ ]] || fail "release checksum has an invalid digest"
[[ "${checksum_file}" == "${archive_name}" && -z "${checksum_extra}" ]] || \
  fail "release checksum does not identify ${archive_name}"
printf '%s  %s\n' "${checksum_digest}" "${archive_name}" >"${verified_checksum_path}"
(
  cd "${download_directory}"
  sha256sum --check --strict "$(basename -- "${verified_checksum_path}")"
)

tar \
  --extract \
  --gzip \
  --no-same-owner \
  --file "${archive_path}" \
  --directory "${download_directory}"

readonly package_root="${download_directory}/${archive_root}"
readonly packaged_installer="${package_root}/packaging/ubuntu/install-taskcaged.sh"
readonly packaged_binary="${package_root}/bin/taskcaged"

[[ -f "${packaged_installer}" && ! -L "${packaged_installer}" ]] || \
  fail "release installer is missing or unsafe"
[[ -f "${packaged_binary}" && ! -L "${packaged_binary}" && -x "${packaged_binary}" ]] || \
  fail "release binary is missing or unsafe"

installer_arguments=(--binary "${packaged_binary}")
if [[ "${start_service}" == true ]]; then
  installer_arguments+=(--start)
fi
bash "${packaged_installer}" "${installer_arguments[@]}"

echo "Installed taskcaged ${release_version} from ${release_url}."
