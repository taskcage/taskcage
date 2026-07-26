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
output_flood_bin="$(pwd)/target/debug/output-flood"
true_bin="$(type -P true)"
false_bin="$(type -P false)"
env_bin="$(type -P env)"
sleep_bin="$(type -P sleep)"
unit_sequence=0
taskcage_limits=(
  --memory-bytes 67108864
  --pids 8
  --cpu-quota-us 50000
  --cpu-period-us 100000
)
taskcage_output_limits=(
  --stdout-tail-bytes 64
  --stderr-tail-bytes 64
)

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
normal_output="$(run_delegated normal run-once \
  --job-id normal \
  "${taskcage_limits[@]}" \
  "${taskcage_output_limits[@]}" \
  --timeout-ms 5000 \
  --working-directory "$(pwd)" \
  -- "${true_bin}")"
grep -q '"exitCode": 0' <<<"${normal_output}"
grep -q '"cleanupComplete": true' <<<"${normal_output}"

nonzero_output="$(run_delegated nonzero run-once \
  --job-id nonzero \
  "${taskcage_limits[@]}" \
  "${taskcage_output_limits[@]}" \
  --timeout-ms 5000 \
  --working-directory "$(pwd)" \
  -- "${false_bin}")"
grep -q '"exitCode": 1' <<<"${nonzero_output}"
grep -q '"cleanupComplete": true' <<<"${nonzero_output}"

# target은 daemon의 PATH를 상속하지 않고 명시한 환경만 받는다.
environment_output="$(run_delegated environment run-once \
  --job-id environment \
  "${taskcage_limits[@]}" \
  "${taskcage_output_limits[@]}" \
  --timeout-ms 5000 \
  --working-directory "$(pwd)" \
  --env TASKCAGE_TEST=explicit \
  -- "${env_bin}")"
grep -q 'TASKCAGE_TEST=explicit' <<<"${environment_output}"
if grep -q 'PATH=' <<<"${environment_output}"; then
  echo "FAIL: target이 daemon PATH를 상속했습니다" >&2
  exit 1
fi

# 대표 프로세스가 먼저 끝나도 남은 자식과 손자를 cgroup 전체 종료로 정리한다.
ghost_output="$(run_delegated ghost run-once \
  --job-id ghost \
  "${taskcage_limits[@]}" \
  "${taskcage_output_limits[@]}" \
  --timeout-ms 5000 \
  --working-directory "$(pwd)" \
  -- "${ghost_bin}")"
grep -q '"membershipVerified": true' <<<"${ghost_output}"
grep -q '"cleanupComplete": true' <<<"${ghost_output}"

# 두 stream을 동시에 끝까지 drain하며 각 stream의 마지막 raw bytes만 독립적으로 남긴다.
flood_both_output="$(run_delegated flood-both run-once \
  --job-id flood-both \
  "${taskcage_limits[@]}" \
  "${taskcage_output_limits[@]}" \
  --timeout-ms 10000 \
  --working-directory "$(pwd)" \
  -- "${output_flood_bin}" both)"
grep -q 'STDOUT-END' <<<"${flood_both_output}"
grep -q 'STDERR-END' <<<"${flood_both_output}"
grep -q '"stdoutTruncated": true' <<<"${flood_both_output}"
grep -q '"stderrTruncated": true' <<<"${flood_both_output}"

flood_stdout_output="$(run_delegated flood-stdout run-once \
  --job-id flood-stdout \
  "${taskcage_limits[@]}" \
  "${taskcage_output_limits[@]}" \
  --timeout-ms 10000 \
  --working-directory "$(pwd)" \
  -- "${output_flood_bin}" stdout)"
grep -q 'STDOUT-END' <<<"${flood_stdout_output}"
grep -q '"stdoutTruncated": true' <<<"${flood_stdout_output}"
grep -q '"stderrTruncated": false' <<<"${flood_stdout_output}"

flood_stderr_output="$(run_delegated flood-stderr run-once \
  --job-id flood-stderr \
  "${taskcage_limits[@]}" \
  "${taskcage_output_limits[@]}" \
  --timeout-ms 10000 \
  --working-directory "$(pwd)" \
  -- "${output_flood_bin}" stderr)"
grep -q 'STDERR-END' <<<"${flood_stderr_output}"
grep -q '"stdoutTruncated": false' <<<"${flood_stderr_output}"
grep -q '"stderrTruncated": true' <<<"${flood_stderr_output}"

# 벽시계 제한을 넘기면 대표 PID가 아니라 작업 cgroup 전체를 끝낸다.
timeout_output="$(run_delegated timeout run-once \
  --job-id timeout \
  "${taskcage_limits[@]}" \
  "${taskcage_output_limits[@]}" \
  --timeout-ms 200 \
  --working-directory "$(pwd)" \
  -- "${sleep_bin}" 30)"
grep -q '"timedOut": true' <<<"${timeout_output}"
grep -q '"cleanupComplete": true' <<<"${timeout_output}"

# 실행 파일과 작업 디렉터리 오류도 제한 없는 실행으로 우회하지 않고 실패해야 한다.
if run_delegated missing run-once \
  --job-id missing \
  "${taskcage_limits[@]}" \
  "${taskcage_output_limits[@]}" \
  --timeout-ms 5000 \
  --working-directory "$(pwd)" \
  -- /definitely/missing/taskcage-target; then
  echo "FAIL: 없는 실행 파일을 성공으로 처리했습니다" >&2
  exit 1
fi
if run_delegated bad-cwd run-once \
  --job-id bad-cwd \
  "${taskcage_limits[@]}" \
  "${taskcage_output_limits[@]}" \
  --timeout-ms 5000 \
  --working-directory /definitely/missing/taskcage-directory \
  -- "${true_bin}"; then
  echo "FAIL: 없는 작업 디렉터리를 성공으로 처리했습니다" >&2
  exit 1
fi

# 검증부터 멱등 예약, 실제 Runner와 FINISHED 저장까지 단일 submit 조정 경로를 통과한다.
submit_test=""
while IFS= read -r artifact; do
  if [[ "${artifact}" == *'"target":{"kind":["lib"]'* &&
        "${artifact}" == *'"test":true'* &&
        "${artifact}" == *'"executable":"'* ]]; then
    submit_test="${artifact#*\"executable\":\"}"
    submit_test="${submit_test%%\"*}"
  fi
done < <(cargo test -p taskcaged --lib --no-run --message-format=json)
if [[ -z "${submit_test}" || ! -x "${submit_test}" ]]; then
  echo "FAIL: submit 조정 경로와 Runner 통합 시험 실행 파일을 찾지 못했습니다" >&2
  exit 1
fi
unit_sequence=$((unit_sequence + 1))
"${taskcage_systemd[@]}" \
  --quiet \
  --wait \
  --collect \
  --pipe \
  --unit="taskcage-submit-runner-$$-${unit_sequence}" \
  --property=Type=exec \
  --property=Delegate=yes \
  --setenv=TASKCAGE_RUN_LINUX_SUBMIT_INTEGRATION=1 \
  "${submit_test}" \
  'submit::tests::actual_submit_coordinator_runs_once_and_finishes_after_cleanup' \
  --exact \
  --nocapture

# typed protocol handler가 기존 capability, submit coordinator와 Registry를 그대로 사용한다.
unit_sequence=$((unit_sequence + 1))
"${taskcage_systemd[@]}" \
  --quiet \
  --wait \
  --collect \
  --pipe \
  --unit="taskcage-protocol-handlers-$$-${unit_sequence}" \
  --property=Type=exec \
  --property=Delegate=yes \
  --setenv=TASKCAGE_RUN_LINUX_HANDLER_INTEGRATION=1 \
  "${submit_test}" \
  'handlers::tests::actual_handlers_connect_submit_and_get_task_to_the_runner' \
  --exact \
  --nocapture
