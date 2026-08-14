#!/usr/bin/env bash
set -euo pipefail

script_directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly script_directory
repository_root="$(cd -- "${script_directory}/.." && pwd)"
readonly repository_root

if [[ $# -gt 1 ]]; then
  echo "ERROR: usage: java-release-consumer-smoke.sh [BUNDLES_DIRECTORY]" >&2
  exit 1
fi

temporary_root="$(mktemp -d /tmp/taskcage-java-release-consumer.XXXXXX)"
resolved_temporary_root="$(readlink -f "${temporary_root}")"
cleanup() {
  if [[ -n "${resolved_temporary_root}" && -d "${resolved_temporary_root}" ]]; then
    case "${resolved_temporary_root}" in
      /tmp/taskcage-java-release-consumer.*|/private/tmp/taskcage-java-release-consumer.*) rm -rf -- "${resolved_temporary_root}" ;;
      *) echo "WARN: refusing to remove unexpected path: ${resolved_temporary_root}" >&2 ;;
    esac
  fi
}
trap cleanup EXIT

requested_bundles_directory="${1:-${temporary_root}/bundles}"
mkdir -p -- "${requested_bundles_directory}"
bundles_directory="$(cd -- "${requested_bundles_directory}" && pwd)"
readonly bundles_directory
readonly repository_directory="${temporary_root}/repository"
mkdir -p -- "${repository_directory}"

release_version() {
  local project_directory="$1"
  local component_name="$2"
  local version

  version="$(
    "${repository_root}/java-sdk/gradlew" \
      -p "${project_directory}" \
      properties \
      --no-daemon \
      --no-problems-report \
      --quiet | \
      awk -F ': ' '$1 == "version" { print $2; exit }'
  )"
  [[ -n "${version}" ]] || {
    echo "ERROR: ${component_name} version could not be read" >&2
    exit 1
  }
  printf '%s\n' "${version}"
}

readonly java_sdk_version="$(release_version "${repository_root}/java-sdk" "java-sdk")"
"${repository_root}/scripts/release/build-validation-central-bundle.sh" \
  "${java_sdk_version}" \
  "${bundles_directory}"

(
  cd "${repository_directory}"
  jar --extract --file "${bundles_directory}/taskcage-java-sdk-${java_sdk_version}-central.zip"
)

TASKCAGE_RELEASE_REPOSITORY="${repository_directory}" \
TASKCAGE_JAVA_SDK_VERSION="${java_sdk_version}" \
  "${repository_root}/java-sdk/gradlew" \
  -p "${repository_root}/integration-tests/java-release-consumer" \
  clean \
  compileJava \
  --no-daemon \
  --no-problems-report

echo "PASS: external Java consumer resolved the Core SDK"
