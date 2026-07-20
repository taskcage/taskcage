#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "SKIP: cgroup smoke test requires Linux" >&2
  exit 77
fi

if ! command -v systemd-run >/dev/null 2>&1; then
  echo "SKIP: systemd-run is unavailable" >&2
  exit 77
fi

if [[ ! -f /sys/fs/cgroup/cgroup.controllers ]]; then
  echo "SKIP: unified cgroup v2 is unavailable" >&2
  exit 77
fi

cargo build --workspace

taskcage_systemd=(systemd-run)
if [[ "${EUID}" -ne 0 ]]; then
  if sudo -n true >/dev/null 2>&1; then
    taskcage_systemd=(sudo -n systemd-run)
  else
    echo "SKIP: root or passwordless sudo is required for a transient service" >&2
    exit 77
  fi
fi

taskcage_unit="taskcage-cgroup-smoke-$$"
taskcage_output="$("${taskcage_systemd[@]}" \
  --quiet \
  --wait \
  --collect \
  --pipe \
  --unit="${taskcage_unit}" \
  --property=Type=exec \
  --property=Delegate=yes \
  "$(pwd)/target/debug/taskcaged" \
  run-once \
  --memory-bytes 67108864 \
  --pids 8 \
  --cpu-quota-us 50000 \
  --cpu-period-us 100000 \
  --timeout-ms 5000 \
  -- \
  "$(pwd)/target/debug/ghost-tree")"

printf '%s\n' "${taskcage_output}"
grep -q '"membershipVerified": true' <<<"${taskcage_output}"
grep -q '"cleanupComplete": true' <<<"${taskcage_output}"

taskcage_timeout_unit="taskcage-timeout-smoke-$$"
taskcage_timeout_output="$("${taskcage_systemd[@]}" \
  --quiet \
  --wait \
  --collect \
  --pipe \
  --unit="${taskcage_timeout_unit}" \
  --property=Type=exec \
  --property=Delegate=yes \
  "$(pwd)/target/debug/taskcaged" \
  run-once \
  --memory-bytes 67108864 \
  --pids 8 \
  --cpu-quota-us 50000 \
  --cpu-period-us 100000 \
  --timeout-ms 200 \
  -- \
  "$(command -v sleep)" 30)"

printf '%s\n' "${taskcage_timeout_output}"
grep -q '"membershipVerified": true' <<<"${taskcage_timeout_output}"
grep -q '"timedOut": true' <<<"${taskcage_timeout_output}"
grep -q '"cleanupComplete": true' <<<"${taskcage_timeout_output}"
