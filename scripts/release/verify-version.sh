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
  fail "usage: verify-version.sh VERSION [TAG]"
fi

readonly release_version="$1"
readonly release_tag="${2:-}"

[[ "${release_version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z]+([.-][0-9A-Za-z]+)*)?$ ]] || \
  fail "invalid release version: ${release_version}"
[[ ! "${release_version}" =~ -SNAPSHOT$ ]] || fail "release version must not be a snapshot"

cd "${repository_root}"
daemon_version="$(sed -n 's/^version = "\([^"]*\)"$/\1/p' daemon/Cargo.toml | head -n 1)"
java_sdk_version="$(sed -n 's/^version = "\([^"]*\)"$/\1/p' java-sdk/build.gradle.kts | head -n 1)"

[[ -n "${daemon_version}" ]] || fail "daemon version could not be read"
[[ -n "${java_sdk_version}" ]] || fail "Java SDK version could not be read"
[[ "${daemon_version}" == "${release_version}" ]] || \
  fail "daemon version ${daemon_version} does not match ${release_version}"
[[ "${java_sdk_version}" == "${release_version}" ]] || \
  fail "Java SDK version ${java_sdk_version} does not match ${release_version}"

if [[ -n "${release_tag}" && "${release_tag}" != "v${release_version}" ]]; then
  fail "tag ${release_tag} does not match v${release_version}"
fi

echo "version=${release_version}"
