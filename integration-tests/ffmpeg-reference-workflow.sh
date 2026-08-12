#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "SKIP: FFmpeg reference workflow는 Linux가 필요합니다" >&2
  exit 77
fi
if [[ ! -f /sys/fs/cgroup/cgroup.controllers || ! -d /run/systemd/system ]]; then
  echo "SKIP: 실제 cgroup v2와 systemd가 필요합니다" >&2
  exit 77
fi
if ! command -v systemctl >/dev/null 2>&1 || ! sudo -n true >/dev/null 2>&1; then
  echo "SKIP: system service를 설치할 비대화형 sudo 권한이 필요합니다" >&2
  exit 77
fi
if ! command -v ffmpeg >/dev/null 2>&1; then
  echo "SKIP: Ubuntu FFmpeg package가 필요합니다" >&2
  exit 77
fi
if ! command -v java >/dev/null 2>&1; then
  echo "SKIP: Java 17 runtime이 필요합니다" >&2
  exit 77
fi
if [[ -e /etc/systemd/system/taskcaged.service || -e /usr/local/bin/taskcaged ]] || \
  id taskcage >/dev/null 2>&1 || getent group taskcage >/dev/null 2>&1; then
  echo "SKIP: 기존 TaskCage 설치나 account를 변경하지 않습니다" >&2
  exit 77
fi

workflow_started_at="$(date +%s)"
reference_root="$(mktemp -d /tmp/taskcage-ffmpeg-reference.XXXXXX)"
resolved_reference_root="$(readlink -f "${reference_root}")"

cleanup() {
  sudo -n packaging/ubuntu/uninstall-taskcaged.sh --purge-config >/dev/null 2>&1 || true
  if [[ -n "${resolved_reference_root}" && -d "${resolved_reference_root}" ]]; then
    case "${resolved_reference_root}" in
      /tmp/taskcage-ffmpeg-reference.*)
        sudo -n rm -rf -- "${resolved_reference_root}"
        ;;
      *)
        echo "WARN: 예상하지 않은 임시 경로는 제거하지 않습니다: ${resolved_reference_root}" >&2
        ;;
    esac
  fi
  sudo -n userdel taskcage >/dev/null 2>&1 || true
  sudo -n groupdel taskcage >/dev/null 2>&1 || true
}

show_failure() {
  local status=$?
  if [[ ${status} -ne 0 ]]; then
    sudo -n systemctl status taskcaged.service --no-pager || true
    sudo -n journalctl -u taskcaged.service --no-pager -n 100 || true
  fi
  cleanup
  exit "${status}"
}
trap show_failure EXIT

cargo build --workspace

ffmpeg_bin="$(readlink -f "$(command -v ffmpeg)")"
java_bin="$(readlink -f "$(command -v java)")"
java_home="$(dirname "$(dirname "${java_bin}")")"
ffmpeg_version="$("${ffmpeg_bin}" -version | head -n 1)"
echo "FFmpeg reference: ${ffmpeg_version}"

sudo -n packaging/ubuntu/install-taskcaged.sh \
  --binary target/debug/taskcaged \
  --start

for _ in {1..100}; do
  if sudo -n test -S /run/taskcage/taskcaged.sock; then
    break
  fi
  sleep 0.1
done

sudo -n systemctl is-active --quiet taskcaged.service
status_json="$(sudo -n -u taskcage /usr/local/bin/taskcaged status \
  --socket /run/taskcage/taskcaged.sock \
  --timeout-ms 2000)"
grep -q '"status":"READY"' <<<"${status_json}"

main_pid="$(systemctl show taskcaged.service --property=MainPID --value)"
[[ "${main_pid}" =~ ^[1-9][0-9]*$ ]]
manager_membership="$(cut -d: -f3 "/proc/${main_pid}/cgroup")"
[[ "${manager_membership}" == */taskcaged.service/manager ]]
delegated_root="/sys/fs/cgroup${manager_membership%/manager}"

count_task_cgroups() {
  if sudo -n test -d "${delegated_root}/jobs"; then
    sudo -n find "${delegated_root}/jobs" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d '[:space:]'
  else
    echo 0
  fi
}

task_cgroups_before="$(count_task_cgroups)"

sudo -n cp -R java-sdk "${reference_root}/java-sdk"
sudo -n cp -R protocol-fixtures "${reference_root}/protocol-fixtures"
sudo -n install -D -m 0755 target/debug/ffmpeg-tree "${reference_root}/bin/ffmpeg-tree"
sudo -n install -d -m 0700 "${reference_root}/home" "${reference_root}/gradle-home" "${reference_root}/work"
"${ffmpeg_bin}" \
  -hide_banner \
  -loglevel error \
  -nostdin \
  -f lavfi \
  -i 'sine=frequency=1000:sample_rate=44100:duration=1' \
  -c:a pcm_s16le \
  "${reference_root}/profile-source.wav"
sudo -n chown -R taskcage:taskcage "${reference_root}"
sudo -n chmod 0700 "${reference_root}"

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
  echo "FAIL: 실제 FFmpeg Profile 통합 시험 실행 파일을 찾지 못했습니다" >&2
  exit 1
fi
profile_test_bin="${reference_root}/bin/taskcaged-profile-tests"
sudo -n install -m 0755 "${submit_test}" "${profile_test_bin}"
sudo -n chown taskcage:taskcage "${profile_test_bin}"

sudo -n systemd-run \
  --quiet \
  --wait \
  --collect \
  --pipe \
  --unit="taskcage-real-ffmpeg-profile-$$" \
  --property=Type=exec \
  --property=Delegate=yes \
  --uid=taskcage \
  --gid=taskcage \
  --setenv=TASKCAGE_RUN_REAL_FFMPEG_PROFILE_INTEGRATION=1 \
  --setenv=TASKCAGE_REAL_FFMPEG_BIN="${ffmpeg_bin}" \
  --setenv=TASKCAGE_REAL_FFMPEG_INPUT="${reference_root}/profile-source.wav" \
  "${profile_test_bin}" \
  'handlers::tests::actual_real_ffmpeg_profile_imports_and_resolves_under_service_uid' \
  --exact \
  --nocapture

sudo -n -u taskcage \
  /bin/bash -c 'cd "$1" && shift && exec "$@"' taskcage-gradle \
  "${reference_root}/java-sdk" \
  /usr/bin/env \
    HOME="${reference_root}/home" \
    GRADLE_USER_HOME="${reference_root}/gradle-home" \
    JAVA_HOME="${java_home}" \
    PATH="${java_home}/bin:/usr/local/bin:/usr/bin:/bin" \
    TASKCAGE_SOCKET=/run/taskcage/taskcaged.sock \
    TASKCAGE_FFMPEG="${ffmpeg_bin}" \
    TASKCAGE_FFMPEG_TREE="${reference_root}/bin/ffmpeg-tree" \
    TASKCAGE_FFMPEG_WORK_DIR="${reference_root}/work" \
    ./gradlew \
    -p "${reference_root}/java-sdk" \
    test ffmpegE2eTest --rerun-tasks --no-daemon

[[ "$(count_task_cgroups)" == "${task_cgroups_before}" ]]
[[ -z "$(sudo -n find "${reference_root}/work" -mindepth 1 -maxdepth 1 -print -quit)" ]]

journal_json="$(sudo -n journalctl -u taskcaged.service --no-pager -o cat)"
grep -q '"event":"task_finished"' <<<"${journal_json}"
grep -q '"termination_reason":"TIMED_OUT"' <<<"${journal_json}"
grep -q '"cleanup_complete":true' <<<"${journal_json}"

elapsed_seconds=$(( $(date +%s) - workflow_started_at ))
if (( elapsed_seconds >= 600 )); then
  echo "FAIL: 설치부터 reference workflow 완료까지 ${elapsed_seconds}초가 걸렸습니다" >&2
  exit 1
fi

trap - EXIT
cleanup
echo "PASS: FFmpeg Local Raw Command 정상 실행, ProcessBuilder descendant 재현, timeout whole-task cleanup을 ${elapsed_seconds}초에 확인했습니다"
