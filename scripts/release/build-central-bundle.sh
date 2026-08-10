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
  fail "usage: build-central-bundle.sh VERSION [OUTPUT_DIRECTORY]"
fi

readonly release_version="$1"
readonly requested_output_directory="${2:-${repository_root}/dist}"
"${script_directory}/verify-version.sh" java-sdk "${release_version}" >/dev/null

[[ -n "${MAVEN_SIGNING_KEY:-}" ]] || fail "MAVEN_SIGNING_KEY is required"
[[ -n "${MAVEN_SIGNING_PASSWORD:-}" ]] || fail "MAVEN_SIGNING_PASSWORD is required"
for required_command in install jar mktemp readlink; do
  command -v "${required_command}" >/dev/null 2>&1 || fail "required command is missing: ${required_command}"
done

mkdir -p -- "${requested_output_directory}"
output_directory="$(cd -- "${requested_output_directory}" && pwd)"
readonly output_directory
readonly bundle_path="${output_directory}/taskcage-java-sdk-${release_version}-central.zip"
[[ ! -e "${bundle_path}" ]] || fail "Central bundle already exists: ${bundle_path}"

bundle_staging_directory="$(mktemp -d /tmp/taskcage-central-bundle.XXXXXX)"
resolved_bundle_staging_directory="$(readlink -f "${bundle_staging_directory}")"
cleanup() {
  if [[ -n "${resolved_bundle_staging_directory}" && -d "${resolved_bundle_staging_directory}" ]]; then
    case "${resolved_bundle_staging_directory}" in
      /tmp/taskcage-central-bundle.*) rm -rf -- "${resolved_bundle_staging_directory}" ;;
      *) echo "WARN: refusing to remove unexpected bundle path: ${resolved_bundle_staging_directory}" >&2 ;;
    esac
  fi
}
trap cleanup EXIT

cd "${repository_root}"
java-sdk/gradlew \
  -p java-sdk \
  clean \
  publishMavenJavaPublicationToCentralBundleRepository \
  --no-daemon \
  --no-problems-report

readonly repository_directory="${repository_root}/java-sdk/build/central-repository"
readonly component_directory="${repository_directory}/io/github/taskcage/taskcage-java-sdk/${release_version}"
readonly artifact_prefix="${component_directory}/taskcage-java-sdk-${release_version}"
readonly staged_component_directory="${bundle_staging_directory}/io/github/taskcage/taskcage-java-sdk/${release_version}"

for artifact in \
  "${artifact_prefix}.jar" \
  "${artifact_prefix}-sources.jar" \
  "${artifact_prefix}-javadoc.jar" \
  "${artifact_prefix}.pom"; do
  [[ -s "${artifact}" ]] || fail "required Maven artifact is missing: ${artifact}"
  [[ -s "${artifact}.asc" ]] || fail "PGP signature is missing: ${artifact}.asc"
  [[ -s "${artifact}.md5" ]] || fail "MD5 checksum is missing: ${artifact}.md5"
  [[ -s "${artifact}.sha1" ]] || fail "SHA-1 checksum is missing: ${artifact}.sha1"
  [[ -s "${artifact}.sha256" ]] || fail "SHA-256 checksum is missing: ${artifact}.sha256"
  [[ -s "${artifact}.sha512" ]] || fail "SHA-512 checksum is missing: ${artifact}.sha512"
  for suffix in "" .asc .md5 .sha1 .sha256 .sha512; do
    install -D -m 0644 "${artifact}${suffix}" \
      "${staged_component_directory}/$(basename -- "${artifact}${suffix}")"
  done
done

jar \
  --create \
  --file "${bundle_path}" \
  --no-manifest \
  -C "${bundle_staging_directory}" \
  io
jar --list --file "${bundle_path}" >/dev/null

echo "central_bundle=${bundle_path}"
