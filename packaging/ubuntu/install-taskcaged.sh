#!/usr/bin/env bash
set -euo pipefail

readonly service_user="taskcage"
readonly service_group="taskcage"
readonly binary_target="/usr/local/bin/taskcaged"
readonly config_directory="/etc/taskcage"
readonly config_target="${config_directory}/taskcaged.env"
readonly trusted_capsules_directory="${config_directory}/trusted-capsules.d"
readonly capsule_cache_directory="/var/lib/taskcage/runtime-package-cache"
readonly unit_target="/etc/systemd/system/taskcaged.service"
readonly script_directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly config_source="${script_directory}/taskcaged.env"
readonly unit_source="${script_directory}/taskcaged.service"

binary_source=""
start_service=false
temporary_binary=""

usage() {
  cat <<'EOF'
Usage: install-taskcaged.sh --binary PATH [--start]

  --binary PATH  Install this prebuilt taskcaged binary.
  --start        Enable and start, or restart, taskcaged.service.
EOF
}

fail() {
  echo "ERROR: $*" >&2
  exit 1
}

cleanup() {
  if [[ -n "${temporary_binary}" ]]; then
    rm -f -- "${temporary_binary}"
  fi
}
trap cleanup EXIT

while [[ $# -gt 0 ]]; do
  case "$1" in
    --binary)
      [[ $# -ge 2 ]] || fail "--binary requires a path"
      [[ -z "${binary_source}" ]] || fail "--binary may be specified only once"
      binary_source="$2"
      shift 2
      ;;
    --start)
      start_service=true
      shift
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
[[ -n "${binary_source}" ]] || fail "--binary is required"
[[ -f "${binary_source}" && -x "${binary_source}" ]] || fail "binary must be a regular executable file: ${binary_source}"
[[ -f "${config_source}" && ! -L "${config_source}" ]] || fail "packaged environment file is missing or unsafe"
[[ -f "${unit_source}" && ! -L "${unit_source}" ]] || fail "packaged systemd unit is missing or unsafe"

for command in getent groupadd id install mktemp mv systemctl useradd; do
  command -v "${command}" >/dev/null 2>&1 || fail "required command is missing: ${command}"
done

if ! getent group "${service_group}" >/dev/null; then
  groupadd --system "${service_group}"
fi

if id "${service_user}" >/dev/null 2>&1; then
  [[ "$(id -gn "${service_user}")" == "${service_group}" ]] || \
    fail "existing ${service_user} user does not use ${service_group} as its primary group"
  service_entry="$(getent passwd "${service_user}")"
  IFS=: read -r _ _ _ _ _ existing_home existing_shell <<<"${service_entry}"
  [[ "${existing_home}" == "/nonexistent" ]] || \
    fail "existing ${service_user} user has an unexpected home: ${existing_home}"
  [[ "${existing_shell}" == "/usr/sbin/nologin" || "${existing_shell}" == "/sbin/nologin" || \
    "${existing_shell}" == "/bin/false" ]] || \
    fail "existing ${service_user} user has a login shell: ${existing_shell}"
else
  [[ -x /usr/sbin/nologin ]] || fail "required nologin shell is missing: /usr/sbin/nologin"
  useradd \
    --system \
    --gid "${service_group}" \
    --home-dir /nonexistent \
    --no-create-home \
    --shell /usr/sbin/nologin \
    "${service_user}"
fi

if [[ -L "${config_directory}" || (-e "${config_directory}" && ! -d "${config_directory}") ]]; then
  fail "configuration path is not a real directory: ${config_directory}"
fi
install -d -o root -g "${service_group}" -m 0750 "${config_directory}"
install -d -o root -g "${service_group}" -m 0750 "${trusted_capsules_directory}"
install -d -o "${service_user}" -g "${service_group}" -m 0700 "${capsule_cache_directory}"

if [[ -L "${config_target}" || (-e "${config_target}" && ! -f "${config_target}") ]]; then
  fail "environment path is not a regular file: ${config_target}"
fi
if [[ ! -e "${config_target}" ]]; then
  install -o root -g "${service_group}" -m 0640 "${config_source}" "${config_target}"
fi

temporary_binary="$(mktemp /usr/local/bin/.taskcaged.XXXXXX)"
install -o root -g root -m 0755 "${binary_source}" "${temporary_binary}"
mv -f -- "${temporary_binary}" "${binary_target}"
temporary_binary=""

install -o root -g root -m 0644 "${unit_source}" "${unit_target}"
systemctl daemon-reload

if [[ "${start_service}" == true ]]; then
  systemctl enable taskcaged.service
  if systemctl is-active --quiet taskcaged.service; then
    systemctl restart taskcaged.service
  else
    systemctl start taskcaged.service
  fi
fi

echo "Installed ${binary_target}, ${config_target}, and ${unit_target}."
if [[ "${start_service}" == false ]]; then
  echo "Review ${config_target}, then run: systemctl enable --now taskcaged.service"
fi
