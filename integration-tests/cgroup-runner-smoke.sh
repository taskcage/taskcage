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
taskcage_systemctl=(systemctl)
if [[ "${EUID}" -ne 0 ]]; then
  if sudo -n true >/dev/null 2>&1; then
    taskcage_systemd=(sudo -n systemd-run)
    taskcage_systemctl=(sudo -n systemctl)
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
touch_bin="$(type -P touch)"
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

# 비정상 종료는 lock FD를 해제하고, 다음 시작은 같은 UID·0600·동일 inode의 stale socket만 제거한다.
"${submit_test}" \
  'startup::tests::abrupt_exit_releases_lock_and_leaves_only_a_recoverable_socket' \
  --exact \
  --nocapture

# stale socket 소유권 획득 뒤 잔여 job 전체를 정리해야 preflight와 UDS 단계로 진행한다.
unit_sequence=$((unit_sequence + 1))
"${taskcage_systemd[@]}" \
  --quiet \
  --wait \
  --collect \
  --pipe \
  --unit="taskcage-startup-cgroup-recovery-$$-${unit_sequence}" \
  --property=Type=exec \
  --property=Delegate=yes \
  --setenv=TASKCAGE_RUN_LINUX_STARTUP_RECOVERY_INTEGRATION=1 \
  --setenv=TASKCAGE_STARTUP_RECOVERY_GHOST_BIN="${ghost_bin}" \
  "${submit_test}" \
  'startup_cgroup::tests::actual_recovery_kills_descendants_removes_jobs_and_allows_preflight' \
  --exact \
  --nocapture

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

# 준비 단계가 길어져도 startedAt과 wallTimeMs는 exec gate commit에서 시작한다.
unit_sequence=$((unit_sequence + 1))
"${taskcage_systemd[@]}" \
  --quiet \
  --wait \
  --collect \
  --pipe \
  --unit="taskcage-task-start-timing-$$-${unit_sequence}" \
  --property=Type=exec \
  --property=Delegate=yes \
  --setenv=TASKCAGE_RUN_LINUX_TASK_TIMING_INTEGRATION=1 \
  --setenv=TASKCAGE_TIMING_MARKER_BIN="${touch_bin}" \
  "${submit_test}" \
  'submit::tests::actual_task_timing_begins_after_exec_gate_commit' \
  --exact \
  --nocapture

# 실제 UDS에서 단일 실행 슬롯을 cancel 응답 직후 다음 submit이 반복 재사용한다.
unit_sequence=$((unit_sequence + 1))
"${taskcage_systemd[@]}" \
  --quiet \
  --wait \
  --collect \
  --pipe \
  --unit="taskcage-uds-server-$$-${unit_sequence}" \
  --property=Type=exec \
  --property=Delegate=yes \
  --setenv=TASKCAGE_RUN_LINUX_UDS_INTEGRATION=1 \
  "${submit_test}" \
  'server::tests::actual_uds_server_runs_disconnect_poll_and_cancel_through_cgroups' \
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

# read-back 불일치의 공개 오류, rollback, capacity와 fail-stop 계약을 실제 cgroup에서 확인한다.
unit_sequence=$((unit_sequence + 1))
"${taskcage_systemd[@]}" \
  --quiet \
  --wait \
  --collect \
  --pipe \
  --unit="taskcage-read-back-contract-$$-${unit_sequence}" \
  --property=Type=exec \
  --property=Delegate=yes \
  --setenv=TASKCAGE_RUN_LINUX_READ_BACK_CONTRACT=1 \
  --setenv=TASKCAGE_READ_BACK_MARKER_BIN="${touch_bin}" \
  "${submit_test}" \
  'handlers::tests::actual_read_back_mismatch_enforces_public_error_and_rollback_contract' \
  --exact \
  --nocapture

# cancelTask는 descendant 전체와 출력 reader를 정리한 뒤에만 CANCELLED를 반환한다.
unit_sequence=$((unit_sequence + 1))
"${taskcage_systemd[@]}" \
  --quiet \
  --wait \
  --collect \
  --pipe \
  --unit="taskcage-task-cancellation-$$-${unit_sequence}" \
  --property=Type=exec \
  --property=Delegate=yes \
  --setenv=TASKCAGE_RUN_LINUX_CANCELLATION_INTEGRATION=1 \
  --setenv=TASKCAGE_GHOST_BIN="${ghost_bin}" \
  "${submit_test}" \
  'handlers::tests::actual_cancel_handler_cleans_descendants_and_preserves_timeout_winner' \
  --exact \
  --nocapture

# membership 확인 뒤 fail-stop이 먼저 끝나면 exec gate를 열지 않고 pending 작업을 정리한다.
unit_sequence=$((unit_sequence + 1))
"${taskcage_systemd[@]}" \
  --quiet \
  --wait \
  --collect \
  --pipe \
  --unit="taskcage-exec-gate-race-$$-${unit_sequence}" \
  --property=Type=exec \
  --property=Delegate=yes \
  --setenv=TASKCAGE_RUN_LINUX_EXEC_GATE_INTEGRATION=1 \
  --setenv=TASKCAGE_EXEC_GATE_GHOST_BIN="${ghost_bin}" \
  "${submit_test}" \
  'submit::tests::actual_fail_stop_before_exec_commit_rolls_back_without_target_start' \
  --exact \
  --nocapture

# exec commit이 먼저 끝난 활성 작업은 fail-stop whole-cgroup 정리 대상에 계속 포함한다.
unit_sequence=$((unit_sequence + 1))
"${taskcage_systemd[@]}" \
  --quiet \
  --wait \
  --collect \
  --pipe \
  --unit="taskcage-fail-stop-$$-${unit_sequence}" \
  --property=Type=exec \
  --property=Delegate=yes \
  --setenv=TASKCAGE_RUN_LINUX_FAIL_STOP_INTEGRATION=1 \
  --setenv=TASKCAGE_FAIL_STOP_GHOST_BIN="${ghost_bin}" \
  "${submit_test}" \
  'submit::tests::actual_fail_stop_cleans_all_active_cgroups_and_blocks_new_execution' \
  --exact \
  --nocapture

# 합성된 실패 보고서가 아니라 실제 child, cgroup과 reader cleanup 경계에서 오류를 발생시킨다.
for cleanup_fault in \
  pending-clone-abort \
  exec-gate-cleanup \
  cgroup-kill \
  direct-child-reap \
  populated-zero \
  statistics \
  cgroup-removal \
  stdout-reader \
  stderr-reader; do
  unit_sequence=$((unit_sequence + 1))
  "${taskcage_systemd[@]}" \
    --quiet \
    --wait \
    --collect \
    --pipe \
    --unit="taskcage-cleanup-fault-${cleanup_fault}-$$-${unit_sequence}" \
    --property=Type=exec \
    --property=Delegate=yes \
    --setenv=TASKCAGE_RUN_CLEANUP_FAULT_INTEGRATION=1 \
    --setenv=TASKCAGE_CLEANUP_FAULT="${cleanup_fault}" \
    --setenv=TASKCAGE_CLEANUP_FAULT_TOUCH_BIN="${touch_bin}" \
    --setenv=TASKCAGE_CLEANUP_FAULT_OUTPUT_BIN="${output_flood_bin}" \
    "${submit_test}" \
    'submit::tests::actual_cleanup_fault_reaches_runner_and_submit_state' \
    --exact \
    --nocapture
done

# 재시도에서도 계속 실패하면 FINISHED와 capacity 재사용이 차단되고 Drop 방어가 자원을 회수한다.
for cleanup_fault in \
  pending-clone-abort \
  cgroup-kill \
  direct-child-reap \
  populated-zero \
  statistics \
  cgroup-removal \
  stdout-reader \
  stderr-reader; do
  unit_sequence=$((unit_sequence + 1))
  "${taskcage_systemd[@]}" \
    --quiet \
    --wait \
    --collect \
    --pipe \
    --unit="taskcage-cleanup-persistent-${cleanup_fault}-$$-${unit_sequence}" \
    --property=Type=exec \
    --property=Delegate=yes \
    --setenv=TASKCAGE_RUN_CLEANUP_FAULT_INTEGRATION=1 \
    --setenv=TASKCAGE_CLEANUP_FAULT="${cleanup_fault}" \
    --setenv=TASKCAGE_CLEANUP_FAULT_MODE=persistent \
    --setenv=TASKCAGE_CLEANUP_FAULT_TOUCH_BIN="${touch_bin}" \
    --setenv=TASKCAGE_CLEANUP_FAULT_OUTPUT_BIN="${output_flood_bin}" \
    "${submit_test}" \
    'submit::tests::actual_cleanup_fault_reaches_runner_and_submit_state' \
    --exact \
    --nocapture
done

# 서로 다른 실제 작업의 cleanup 실패가 동시에 발생해도 최초 fail-stop deadline 하나만 유지한다.
unit_sequence=$((unit_sequence + 1))
"${taskcage_systemd[@]}" \
  --quiet \
  --wait \
  --collect \
  --pipe \
  --unit="taskcage-cleanup-concurrent-$$-${unit_sequence}" \
  --property=Type=exec \
  --property=Delegate=yes \
  --setenv=TASKCAGE_RUN_CLEANUP_FAULT_CONCURRENT=1 \
  "${submit_test}" \
  'submit::tests::concurrent_actual_cleanup_faults_share_one_fail_stop_deadline' \
  --exact \
  --nocapture

# 실제 serve process가 UDS 요청 중 fail-stop에 들어가 deadline 안에 0이 아닌 코드로 종료한다.
process_e2e_test=""
while IFS= read -r artifact; do
  if [[ "${artifact}" == *'"target":{"kind":["test"]'* &&
        "${artifact}" == *'"name":"fail_stop_process_e2e"'* &&
        "${artifact}" == *'"executable":"'* ]]; then
    process_e2e_test="${artifact#*\"executable\":\"}"
    process_e2e_test="${process_e2e_test%%\"*}"
  fi
done < <(cargo test -p taskcaged --test fail_stop_process_e2e --no-run --message-format=json)
if [[ -z "${process_e2e_test}" || ! -x "${process_e2e_test}" ]]; then
  echo "FAIL: 실제 fail-stop process E2E 실행 파일을 찾지 못했습니다" >&2
  exit 1
fi

unit_sequence=$((unit_sequence + 1))
fail_stop_process_unit="taskcage-fail-stop-process-$$-${unit_sequence}"
cleanup_fail_stop_process_unit() {
  "${taskcage_systemctl[@]}" stop "${fail_stop_process_unit}" >/dev/null 2>&1 || true
  "${taskcage_systemctl[@]}" reset-failed "${fail_stop_process_unit}" >/dev/null 2>&1 || true
}
trap cleanup_fail_stop_process_unit EXIT INT TERM
"${taskcage_systemd[@]}" \
  --quiet \
  --wait \
  --collect \
  --pipe \
  --unit="${fail_stop_process_unit}" \
  --property=Type=exec \
  --property=Delegate=yes \
  --property=TimeoutStopSec=10s \
  --setenv=TASKCAGE_RUN_FAIL_STOP_PROCESS_E2E=1 \
  --setenv=TASKCAGE_FAIL_STOP_PROCESS_BIN="${taskcage_bin}" \
  --setenv=TASKCAGE_FAIL_STOP_PROCESS_GHOST_BIN="${ghost_bin}" \
  "${process_e2e_test}" \
  'actual_serve_process_exits_nonzero_after_fail_stop_deadline' \
  --exact \
  --nocapture
trap - EXIT INT TERM

# 정상 shutdown이 먼저 선택된 뒤 cleanup 불확실성이 발생해도 같은 fail-stop deadline으로 비정상 종료한다.
unit_sequence=$((unit_sequence + 1))
shutdown_fail_stop_unit="taskcage-shutdown-fail-stop-$$-${unit_sequence}"
cleanup_shutdown_fail_stop_unit() {
  "${taskcage_systemctl[@]}" stop "${shutdown_fail_stop_unit}" >/dev/null 2>&1 || true
  "${taskcage_systemctl[@]}" reset-failed "${shutdown_fail_stop_unit}" >/dev/null 2>&1 || true
}
trap cleanup_shutdown_fail_stop_unit EXIT INT TERM
"${taskcage_systemd[@]}" \
  --quiet \
  --wait \
  --collect \
  --pipe \
  --unit="${shutdown_fail_stop_unit}" \
  --property=Type=exec \
  --property=Delegate=yes \
  --property=TimeoutStopSec=12s \
  --setenv=TASKCAGE_RUN_FAIL_STOP_PROCESS_E2E=1 \
  --setenv=TASKCAGE_FAIL_STOP_PROCESS_BIN="${taskcage_bin}" \
  --setenv=TASKCAGE_FAIL_STOP_PROCESS_GHOST_BIN="${ghost_bin}" \
  "${process_e2e_test}" \
  'actual_shutdown_drain_switches_to_fail_stop_and_exits_nonzero' \
  --exact \
  --nocapture
trap - EXIT INT TERM

# 실제 serve process에서 UDS handler와 file descriptor가 명시한 연결 상한을 넘지 않는지 확인한다.
uds_connection_limit_test=""
while IFS= read -r artifact; do
  if [[ "${artifact}" == *'"target":{"kind":["test"]'* &&
        "${artifact}" == *'"name":"uds_connection_limit_e2e"'* &&
        "${artifact}" == *'"executable":"'* ]]; then
    uds_connection_limit_test="${artifact#*\"executable\":\"}"
    uds_connection_limit_test="${uds_connection_limit_test%%\"*}"
  fi
done < <(cargo test -p taskcaged --test uds_connection_limit_e2e --no-run --message-format=json)
if [[ -z "${uds_connection_limit_test}" || ! -x "${uds_connection_limit_test}" ]]; then
  echo "FAIL: 실제 UDS connection limit E2E 실행 파일을 찾지 못했습니다" >&2
  exit 1
fi

unit_sequence=$((unit_sequence + 1))
uds_connection_limit_unit="taskcage-uds-connection-limit-$$-${unit_sequence}"
cleanup_uds_connection_limit_unit() {
  "${taskcage_systemctl[@]}" stop "${uds_connection_limit_unit}" >/dev/null 2>&1 || true
  "${taskcage_systemctl[@]}" reset-failed "${uds_connection_limit_unit}" >/dev/null 2>&1 || true
}
trap cleanup_uds_connection_limit_unit EXIT INT TERM
"${taskcage_systemd[@]}" \
  --quiet \
  --wait \
  --collect \
  --pipe \
  --unit="${uds_connection_limit_unit}" \
  --property=Type=exec \
  --property=Delegate=yes \
  --property=TimeoutStopSec=10s \
  --setenv=TASKCAGE_RUN_UDS_CONNECTION_LIMIT_E2E=1 \
  --setenv=TASKCAGE_UDS_CONNECTION_LIMIT_BIN="${taskcage_bin}" \
  "${uds_connection_limit_test}" \
  'actual_serve_process_bounds_uds_connections_and_reuses_slots' \
  --exact \
  --nocapture
trap - EXIT INT TERM

# 실제 serve process가 FINISHED 보존을 포함한 Registry 작업 수를 제한하고 새 target을 만들지 않는다.
registry_capacity_test=""
while IFS= read -r artifact; do
  if [[ "${artifact}" == *'"target":{"kind":["test"]'* &&
        "${artifact}" == *'"name":"registry_capacity_e2e"'* &&
        "${artifact}" == *'"executable":"'* ]]; then
    registry_capacity_test="${artifact#*\"executable\":\"}"
    registry_capacity_test="${registry_capacity_test%%\"*}"
  fi
done < <(cargo test -p taskcaged --test registry_capacity_e2e --no-run --message-format=json)
if [[ -z "${registry_capacity_test}" || ! -x "${registry_capacity_test}" ]]; then
  echo "FAIL: 실제 Registry capacity E2E 실행 파일을 찾지 못했습니다" >&2
  exit 1
fi

unit_sequence=$((unit_sequence + 1))
registry_capacity_unit="taskcage-registry-capacity-$$-${unit_sequence}"
cleanup_registry_capacity_unit() {
  "${taskcage_systemctl[@]}" stop "${registry_capacity_unit}" >/dev/null 2>&1 || true
  "${taskcage_systemctl[@]}" reset-failed "${registry_capacity_unit}" >/dev/null 2>&1 || true
}
trap cleanup_registry_capacity_unit EXIT INT TERM
"${taskcage_systemd[@]}" \
  --quiet \
  --wait \
  --collect \
  --pipe \
  --unit="${registry_capacity_unit}" \
  --property=Type=exec \
  --property=Delegate=yes \
  --property=TimeoutStopSec=10s \
  --setenv=TASKCAGE_RUN_REGISTRY_CAPACITY_E2E=1 \
  --setenv=TASKCAGE_REGISTRY_CAPACITY_BIN="${taskcage_bin}" \
  --setenv=TASKCAGE_REGISTRY_CAPACITY_TRUE_BIN="${true_bin}" \
  --setenv=TASKCAGE_REGISTRY_CAPACITY_TOUCH_BIN="${touch_bin}" \
  "${registry_capacity_test}" \
  'actual_serve_process_bounds_registry_without_execution_side_effects' \
  --exact \
  --nocapture
trap - EXIT INT TERM

# 실제 daemon crash 뒤 stale socket과 잔여 실행을 다음 serve process가 요청 수락 전에 복구한다.
restart_recovery_test=""
while IFS= read -r artifact; do
  if [[ "${artifact}" == *'"target":{"kind":["test"]'* &&
        "${artifact}" == *'"name":"restart_recovery_e2e"'* &&
        "${artifact}" == *'"executable":"'* ]]; then
    restart_recovery_test="${artifact#*\"executable\":\"}"
    restart_recovery_test="${restart_recovery_test%%\"*}"
  fi
done < <(cargo test -p taskcaged --test restart_recovery_e2e --no-run --message-format=json)
if [[ -z "${restart_recovery_test}" || ! -x "${restart_recovery_test}" ]]; then
  echo "FAIL: 실제 restart recovery E2E 실행 파일을 찾지 못했습니다" >&2
  exit 1
fi

unit_sequence=$((unit_sequence + 1))
restart_recovery_unit="taskcage-restart-recovery-$$-${unit_sequence}"
cleanup_restart_recovery_unit() {
  "${taskcage_systemctl[@]}" stop "${restart_recovery_unit}" >/dev/null 2>&1 || true
  "${taskcage_systemctl[@]}" reset-failed "${restart_recovery_unit}" >/dev/null 2>&1 || true
}
trap cleanup_restart_recovery_unit EXIT INT TERM
"${taskcage_systemd[@]}" \
  --quiet \
  --wait \
  --collect \
  --pipe \
  --unit="${restart_recovery_unit}" \
  --property=Type=exec \
  --property=Delegate=yes \
  --property=TimeoutStopSec=10s \
  --setenv=TASKCAGE_RUN_RESTART_RECOVERY_E2E=1 \
  --setenv=TASKCAGE_RESTART_RECOVERY_BIN="${taskcage_bin}" \
  --setenv=TASKCAGE_RESTART_RECOVERY_GHOST_BIN="${ghost_bin}" \
  "${restart_recovery_test}" \
  'actual_restart_recovers_stale_socket_and_residual_execution' \
  --exact \
  --nocapture
trap - EXIT INT TERM
