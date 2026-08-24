#!/usr/bin/env bash
set -euo pipefail

readonly binary_target="/usr/local/bin/taskcaged"
readonly config_directory="/etc/taskcage"
readonly config_target="${config_directory}/taskcaged.env"
readonly trusted_capsules_directory="${config_directory}/trusted-capsules.d"
readonly unit_target="/etc/systemd/system/taskcaged.service"

purge_config=false

usage() {
  cat <<'EOF'
Usage: uninstall-taskcaged.sh [--purge-config]

The taskcage service account and group are preserved. Use --purge-config to
also remove /etc/taskcage/taskcaged.env when the directory is otherwise empty.
EOF
}

fail() {
  echo "ERROR: $*" >&2
  exit 1
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --purge-config)
      purge_config=true
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

[[ "${EUID}" -eq 0 ]] || fail "run this uninstaller as root"
command -v systemctl >/dev/null 2>&1 || fail "required command is missing: systemctl"

systemctl disable --now taskcaged.service >/dev/null 2>&1 || true
rm -f -- "${unit_target}" "${binary_target}"
systemctl daemon-reload
systemctl reset-failed taskcaged.service >/dev/null 2>&1 || true

if [[ "${purge_config}" == true ]]; then
  rm -f -- "${config_target}"
  rmdir -- "${trusted_capsules_directory}" 2>/dev/null || true
  rmdir -- "${config_directory}" 2>/dev/null || true
  echo "Removed TaskCage service assets and configuration. The taskcage account was preserved."
else
  echo "Removed TaskCage service assets. Preserved ${config_target} and the taskcage account."
fi
