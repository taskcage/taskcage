#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "SKIP: 실제 사전 검사는 Linux가 필요합니다" >&2
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

# 실제 커널에서 위임 경로, manager 이동, 제어기, cgroup.kill과 clone3 원자 진입을 확인한다.
taskcage_unit="taskcage-preflight-$$"
taskcage_output="$("${taskcage_systemd[@]}" \
  --quiet \
  --wait \
  --collect \
  --pipe \
  --unit="${taskcage_unit}" \
  --property=Type=exec \
  --property=Delegate=yes \
  "$(pwd)/target/debug/taskcaged" check-environment)"

printf '%s\n' "${taskcage_output}"
grep -q '"managerMembershipVerified":true' <<<"${taskcage_output}"
grep -q '"atomicEntrySupported":true' <<<"${taskcage_output}"

# 일반 디렉터리를 cgroup 경로로 주면 manager나 사용자 프로그램을 만들기 전에 실패해야 한다.
wrong_root="$(mktemp -d)"
trap 'rmdir "${wrong_root}" 2>/dev/null || true' EXIT
if TASKCAGE_CGROUP_ROOT="${wrong_root}" \
  "$(pwd)/target/debug/taskcaged" check-environment >/dev/null 2>&1; then
  echo "FAIL: 일반 디렉터리를 cgroup v2로 받아들였습니다" >&2
  exit 1
fi
if find "${wrong_root}" -mindepth 1 -print -quit | grep -q .; then
  echo "FAIL: 잘못된 경로 안에 파일이나 cgroup을 만들었습니다" >&2
  exit 1
fi

# 재현하기 어려운 controller·권한·clone3 실패는 같은 실행 차단 함수를 가짜 검사로 검증한다.
cargo test --package taskcaged --test preflight_fail_closed
