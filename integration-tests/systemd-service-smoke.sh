#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "SKIP: systemd service 시험은 Linux가 필요합니다" >&2
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
if [[ -e /etc/systemd/system/taskcaged.service || -e /usr/local/bin/taskcaged ]] || \
  id taskcage >/dev/null 2>&1 || getent group taskcage >/dev/null 2>&1; then
  echo "SKIP: 기존 TaskCage 설치나 account를 변경하지 않습니다" >&2
  exit 77
fi

cleanup() {
  sudo -n packaging/ubuntu/uninstall-taskcaged.sh --purge-config >/dev/null 2>&1 || true
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

sudo -n packaging/ubuntu/install-taskcaged.sh \
  --binary target/debug/taskcaged \
  --start
sudo -n systemd-analyze verify taskcaged.service

for _ in {1..100}; do
  if sudo -n test -S /run/taskcage/taskcaged.sock; then
    break
  fi
  sleep 0.1
done

sudo -n systemctl is-active --quiet taskcaged.service
[[ "$(systemctl show taskcaged.service --property=User --value)" == "taskcage" ]]
[[ "$(systemctl show taskcaged.service --property=Group --value)" == "taskcage" ]]
[[ "$(systemctl show taskcaged.service --property=Delegate --value)" == "yes" ]]
[[ "$(sudo -n stat -c '%a %U %G' /run/taskcage/taskcaged.sock)" == "600 taskcage taskcage" ]]

status_json="$(sudo -n -u taskcage /usr/local/bin/taskcaged status \
  --socket /run/taskcage/taskcaged.sock \
  --timeout-ms 2000)"
grep -q '"status":"READY"' <<<"${status_json}"
grep -q '"cgroupV2Ready":true' <<<"${status_json}"

journal_json="$(sudo -n journalctl -u taskcaged.service --no-pager -o cat)"
grep -q '"event":"daemon_started"' <<<"${journal_json}"
grep -q '"event":"status_reported"' <<<"${journal_json}"
grep -q '"operation":"getCapabilities"' <<<"${journal_json}"

main_pid="$(systemctl show taskcaged.service --property=MainPID --value)"
[[ "${main_pid}" =~ ^[1-9][0-9]*$ ]]
manager_membership="$(cut -d: -f3 "/proc/${main_pid}/cgroup")"
[[ "${manager_membership}" == */taskcaged.service/manager ]]
delegated_root="/sys/fs/cgroup${manager_membership%/manager}"
for controller in cpu memory pids; do
  grep -qw "${controller}" "${delegated_root}/cgroup.subtree_control"
done

echo '# operator-preserved-marker' | sudo -n tee -a /etc/taskcage/taskcaged.env >/dev/null
sudo -n packaging/ubuntu/install-taskcaged.sh --binary target/debug/taskcaged
sudo -n grep -q '^# operator-preserved-marker$' /etc/taskcage/taskcaged.env

sudo -n systemctl stop taskcaged.service
[[ "$(systemctl is-active taskcaged.service)" == "inactive" ]]
[[ ! -e /run/taskcage/taskcaged.sock ]]
[[ ! -d /run/taskcage ]]

sudo -n packaging/ubuntu/uninstall-taskcaged.sh
[[ ! -e /usr/local/bin/taskcaged ]]
[[ ! -e /etc/systemd/system/taskcaged.service ]]
sudo -n test -f /etc/taskcage/taskcaged.env
id taskcage >/dev/null

sudo -n packaging/ubuntu/uninstall-taskcaged.sh --purge-config
sudo -n test ! -e /etc/taskcage/taskcaged.env

trap - EXIT
cleanup
echo "PASS: Ubuntu systemd service 설치, readiness, 구조화 log, 위임, 재설치, 종료와 제거를 확인했습니다"
