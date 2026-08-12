#!/usr/bin/env bash
set -euo pipefail

script_directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly script_directory

fail() {
  echo "ERROR: $*" >&2
  exit 1
}

if [[ $# -lt 1 || $# -gt 2 ]]; then
  fail "usage: build-validation-ffmpeg-binding-central-bundle.sh VERSION [OUTPUT_DIRECTORY]"
fi
command -v gpg >/dev/null 2>&1 || fail "required command is missing: gpg"

readonly release_version="$1"
readonly output_directory="${2:-}"
validation_key_home="$(mktemp -d /tmp/taskcage-ffmpeg-binding-release-key.XXXXXX)"
resolved_validation_key_home="$(readlink -f "${validation_key_home}")"

cleanup() {
  if [[ -n "${resolved_validation_key_home}" && -d "${resolved_validation_key_home}" ]]; then
    case "${resolved_validation_key_home}" in
      /tmp/taskcage-ffmpeg-binding-release-key.*|/private/tmp/taskcage-ffmpeg-binding-release-key.*) rm -rf -- "${resolved_validation_key_home}" ;;
      *) echo "WARN: refusing to remove unexpected key path: ${resolved_validation_key_home}" >&2 ;;
    esac
  fi
}
trap cleanup EXIT

chmod 0700 "${validation_key_home}"
export GNUPGHOME="${validation_key_home}"
export MAVEN_SIGNING_PASSWORD="taskcage-release-validation"

gpg \
  --batch \
  --pinentry-mode loopback \
  --passphrase "${MAVEN_SIGNING_PASSWORD}" \
  --quick-generate-key \
  "TaskCage release validation <release-validation@taskcage.invalid>" \
  rsa2048 \
  sign \
  1d >/dev/null 2>&1

validation_fingerprint="$(gpg --batch --with-colons --list-secret-keys | \
  awk -F: '$1 == "fpr" { print $10; exit }')"
[[ -n "${validation_fingerprint}" ]] || fail "validation signing key fingerprint is missing"

MAVEN_SIGNING_KEY="$(gpg \
  --batch \
  --pinentry-mode loopback \
  --passphrase "${MAVEN_SIGNING_PASSWORD}" \
  --armor \
  --export-secret-keys "${validation_fingerprint}")"
export MAVEN_SIGNING_KEY

if [[ -n "${output_directory}" ]]; then
  "${script_directory}/build-ffmpeg-binding-central-bundle.sh" "${release_version}" "${output_directory}"
else
  "${script_directory}/build-ffmpeg-binding-central-bundle.sh" "${release_version}"
fi
