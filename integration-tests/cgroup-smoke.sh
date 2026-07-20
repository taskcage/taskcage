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

# 실제 커널의 cgroup v2 제어 파일이 있어야 한다. 일반 디렉터리로 흉내 낸 시험은 인정하지 않는다.
if [[ ! -f /sys/fs/cgroup/cgroup.controllers ]]; then
  echo "SKIP: unified cgroup v2 is unavailable" >&2
  exit 77
fi

cargo build --workspace

# 다른 cgroup에 영향을 주지 않도록 이 시험만을 위한 일회성 systemd 서비스를 만든다.
# 일반 사용자로 실행할 때는 암호 입력 없이 권한을 얻을 수 있는 환경에서만 계속한다.
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
# 자식과 손자 프로세스를 만드는 시험 프로그램을 실행한다. 대표 프로세스가 먼저 끝나도
# 남은 프로세스가 모두 정리되고 작업 cgroup이 비워지는지 결과값으로 확인한다.
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
# 30초 동안 실행되는 명령에 200밀리초 제한을 걸어, 시간 초과 감지와 전체 정리를 확인한다.
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
