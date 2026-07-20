#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "SKIP: 실제 작업 실행 시험은 Linux가 필요합니다" >&2
  exit 77
fi
if [[ ! -f /sys/fs/cgroup/cgroup.controllers ]]; then
  echo "SKIP: cgroup v2를 찾지 못했습니다" >&2
  exit 77
fi
if ! command -v systemd-run >/dev/null 2>&1; then
  echo "SKIP: systemd-run을 찾지 못했습니다" >&2
  exit 77
fi

taskcage_systemd=(systemd-run)
if [[ "${EUID}" -ne 0 ]]; then
  if sudo -n true >/dev/null 2>&1; then
    taskcage_systemd=(sudo -n systemd-run)
  else
    echo "SKIP: 일회성 위임 서비스를 만들 권한이 없습니다" >&2
    exit 77
  fi
fi

cargo build --workspace
taskcage_bin="$(pwd)/target/debug/taskcaged"
ghost_bin="$(pwd)/target/debug/ghost-tree"
unit_sequence=0

run_delegated() {
  local label="$1"
  shift
  unit_sequence=$((unit_sequence + 1))
  "${taskcage_systemd[@]}" \
    --quiet \
    --wait \
    --collect \
    --pipe \
    --unit="taskcage-runner-${label}-$$-${unit_sequence}" \
    --property=Type=exec \
    --property=Delegate=yes \
    "${taskcage_bin}" "$@"
}

# 정상 종료와 0이 아닌 종료를 그대로 결과에 남긴다.
normal_output="$(run_delegated normal run-once --job-id normal -- "$(command -v true)")"
grep -q '"exitCode": 0' <<<"${normal_output}"
grep -q '"cleanupComplete": true' <<<"${normal_output}"

nonzero_output="$(run_delegated nonzero run-once --job-id nonzero -- "$(command -v false)")"
grep -q '"exitCode": 1' <<<"${nonzero_output}"
grep -q '"cleanupComplete": true' <<<"${nonzero_output}"

# 대표 프로세스가 먼저 끝나도 남은 자식과 손자를 cgroup 전체 종료로 정리한다.
ghost_output="$(run_delegated ghost run-once \
  --job-id ghost \
  --memory-bytes 67108864 \
  --pids 8 \
  --cpu-quota-us 50000 \
  --cpu-period-us 100000 \
  --timeout-ms 5000 \
  -- "${ghost_bin}")"
grep -q '"membershipVerified": true' <<<"${ghost_output}"
grep -q '"cleanupComplete": true' <<<"${ghost_output}"

# 벽시계 제한을 넘기면 대표 PID가 아니라 작업 cgroup 전체를 끝낸다.
timeout_output="$(run_delegated timeout run-once \
  --job-id timeout \
  --memory-bytes 67108864 \
  --pids 8 \
  --cpu-quota-us 50000 \
  --cpu-period-us 100000 \
  --timeout-ms 200 \
  -- "$(command -v sleep)" 30)"
grep -q '"timedOut": true' <<<"${timeout_output}"
grep -q '"cleanupComplete": true' <<<"${timeout_output}"

# 실행 파일과 작업 디렉터리 오류도 제한 없는 실행으로 우회하지 않고 실패해야 한다.
if run_delegated missing run-once --job-id missing -- /definitely/missing/taskcage-target; then
  echo "FAIL: 없는 실행 파일을 성공으로 처리했습니다" >&2
  exit 1
fi
if run_delegated bad-cwd run-once \
  --job-id bad-cwd \
  --working-directory /definitely/missing/taskcage-directory \
  -- "$(command -v true)"; then
  echo "FAIL: 없는 작업 디렉터리를 성공으로 처리했습니다" >&2
  exit 1
fi
