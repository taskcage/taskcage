#!/usr/bin/env bash
set -euo pipefail

readonly central_base_url="https://central.sonatype.com/api/v1/publisher"

fail() {
  echo "ERROR: $*" >&2
  exit 1
}

[[ -n "${CENTRAL_USERNAME:-}" ]] || fail "CENTRAL_USERNAME is required"
[[ -n "${CENTRAL_PASSWORD:-}" ]] || fail "CENTRAL_PASSWORD is required"
command -v base64 >/dev/null 2>&1 || fail "required command is missing: base64"
command -v curl >/dev/null 2>&1 || fail "required command is missing: curl"

central_token="$(printf '%s:%s' "${CENTRAL_USERNAME}" "${CENTRAL_PASSWORD}" | base64 | tr -d '\r\n')"
readonly authorization_header="Authorization: Bearer ${central_token}"

case "${1:-}" in
  upload)
    [[ $# -eq 3 ]] || fail "usage: central-portal.sh upload BUNDLE DEPLOYMENT_NAME"
    readonly bundle_path="$2"
    readonly deployment_name="$3"
    [[ -f "${bundle_path}" && ! -L "${bundle_path}" ]] || fail "bundle must be a regular file"
    [[ "${deployment_name}" =~ ^[0-9A-Za-z._-]+$ ]] || fail "deployment name contains unsafe characters"
    deployment_id="$(curl \
      --fail-with-body \
      --silent \
      --show-error \
      --request POST \
      --header "${authorization_header}" \
      --form "bundle=@${bundle_path};type=application/octet-stream" \
      "${central_base_url}/upload?name=${deployment_name}&publishingType=USER_MANAGED")"
    [[ "${deployment_id}" =~ ^[0-9a-fA-F-]{36}$ ]] || fail "Central returned an invalid deployment ID"
    printf '%s\n' "${deployment_id}"
    ;;
  status)
    [[ $# -eq 2 ]] || fail "usage: central-portal.sh status DEPLOYMENT_ID"
    readonly deployment_id="$2"
    [[ "${deployment_id}" =~ ^[0-9a-fA-F-]{36}$ ]] || fail "invalid deployment ID"
    curl \
      --fail-with-body \
      --silent \
      --show-error \
      --request POST \
      --header "${authorization_header}" \
      "${central_base_url}/status?id=${deployment_id}"
    ;;
  publish)
    [[ $# -eq 2 ]] || fail "usage: central-portal.sh publish DEPLOYMENT_ID"
    readonly deployment_id="$2"
    [[ "${deployment_id}" =~ ^[0-9a-fA-F-]{36}$ ]] || fail "invalid deployment ID"
    curl \
      --fail-with-body \
      --silent \
      --show-error \
      --request POST \
      --header "${authorization_header}" \
      --output /dev/null \
      "${central_base_url}/deployment/${deployment_id}"
    ;;
  *)
    fail "usage: central-portal.sh {upload|status|publish} ..."
    ;;
esac
