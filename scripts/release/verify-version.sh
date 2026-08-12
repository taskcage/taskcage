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

if [[ $# -lt 2 || $# -gt 3 ]]; then
  fail "usage: verify-version.sh COMPONENT VERSION [TAG]"
fi

readonly release_component="$1"
readonly release_version="$2"
readonly release_tag="${3:-}"
readonly semantic_version_pattern='^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$'

[[ "${release_version}" =~ ${semantic_version_pattern} ]] || \
  fail "invalid release version: ${release_version}"

cd "${repository_root}"
case "${release_component}" in
  taskcaged)
    manifest_version="$(sed -n 's/^version = "\([^"]*\)"$/\1/p' daemon/Cargo.toml | head -n 1)"
    ;;
  java-sdk)
    manifest_version="$(sed -n 's/^version = "\([^"]*\)"$/\1/p' java-sdk/build.gradle.kts | head -n 1)"
    ;;
  ffmpeg-binding)
    manifest_version="$(sed -n 's/^version = "\([^"]*\)"$/\1/p' java-bindings/ffmpeg/build.gradle.kts | head -n 1)"
    ;;
  *)
    fail "unsupported release component: ${release_component}"
    ;;
esac

[[ -n "${manifest_version}" ]] || fail "${release_component} version could not be read"
[[ "${manifest_version}" == "${release_version}" ]] || \
  fail "${release_component} version ${manifest_version} does not match ${release_version}"

readonly expected_tag="${release_component}-v${release_version}"
if [[ -n "${release_tag}" && "${release_tag}" != "${expected_tag}" ]]; then
  fail "tag ${release_tag} does not match ${expected_tag}"
fi

echo "component=${release_component}"
echo "version=${release_version}"
