#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "SKIP: release artifact 시험은 Linux가 필요합니다" >&2
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
for required_command in python3 readlink sha256sum tar; do
  command -v "${required_command}" >/dev/null 2>&1 || {
    echo "ERROR: required command is missing: ${required_command}" >&2
    exit 1
  }
done
if [[ $# -ne 4 ]]; then
  echo "ERROR: usage: release-artifact-smoke.sh VERSION ARCHIVE CHECKSUM BOOTSTRAP_INSTALLER" >&2
  exit 1
fi
if [[ -e /etc/systemd/system/taskcaged.service || -e /usr/local/bin/taskcaged ]] || \
  id taskcage >/dev/null 2>&1 || getent group taskcage >/dev/null 2>&1; then
  echo "SKIP: 기존 TaskCage 설치나 account를 변경하지 않습니다" >&2
  exit 77
fi

readonly release_version="$1"
archive_path="$(readlink -f "$2")"
readonly archive_path
checksum_path="$(readlink -f "$3")"
readonly checksum_path
bootstrap_installer_path="$(readlink -f "$4")"
readonly bootstrap_installer_path
case "$(uname -m)" in
  x86_64) release_target="x86_64-unknown-linux-gnu" ;;
  aarch64) release_target="aarch64-unknown-linux-gnu" ;;
  *)
    echo "SKIP: release artifact 시험은 Linux x86_64 또는 aarch64가 필요합니다" >&2
    exit 77
    ;;
esac
readonly release_target
readonly archive_root="taskcage-v${release_version}-${release_target}"

[[ -f "${archive_path}" && ! -L "${archive_path}" ]] || {
  echo "ERROR: release archive must be a regular file" >&2
  exit 1
}
[[ -f "${checksum_path}" && ! -L "${checksum_path}" ]] || {
  echo "ERROR: release checksum must be a regular file" >&2
  exit 1
}
[[ -f "${bootstrap_installer_path}" && ! -L "${bootstrap_installer_path}" ]] || {
  echo "ERROR: release bootstrap installer must be a regular file" >&2
  exit 1
}
bash -n "${bootstrap_installer_path}"

(
  cd "$(dirname -- "${archive_path}")"
  sha256sum --check "$(basename -- "${checksum_path}")"
)

python3 - "${archive_path}" "${archive_root}" <<'PY'
import sys
import tarfile

archive_path, root = sys.argv[1:]
required_files = {
    f"{root}/bin/taskcaged",
    f"{root}/packaging/ubuntu/install-taskcaged.sh",
    f"{root}/packaging/ubuntu/uninstall-taskcaged.sh",
    f"{root}/packaging/ubuntu/taskcaged.env",
    f"{root}/packaging/ubuntu/taskcaged.service",
    f"{root}/README.md",
    f"{root}/LICENSE",
}
allowed_directories = {
    root,
    f"{root}/bin",
    f"{root}/packaging",
    f"{root}/packaging/ubuntu",
}
seen = set()
with tarfile.open(archive_path, "r:gz") as archive:
    for member in archive.getmembers():
        name = member.name.rstrip("/")
        if not name or name.startswith("/") or ".." in name.split("/"):
            raise SystemExit(f"ERROR: unsafe archive entry: {member.name}")
        if name in seen:
            raise SystemExit(f"ERROR: duplicate archive entry: {member.name}")
        seen.add(name)
        if member.isdir() and name in allowed_directories:
            continue
        if member.isfile() and name in required_files:
            continue
        raise SystemExit(f"ERROR: unexpected archive entry or type: {member.name}")

missing = required_files - seen
if missing:
    raise SystemExit(f"ERROR: archive is missing required files: {sorted(missing)}")
PY

test_root="$(mktemp -d /tmp/taskcage-release-smoke.XXXXXX)"
resolved_test_root="$(readlink -f "${test_root}")"
package_root="${test_root}/${archive_root}"
installation_attempted=false

cleanup() {
  if [[ "${installation_attempted}" == "true" && -x "${package_root}/packaging/ubuntu/uninstall-taskcaged.sh" ]]; then
    sudo -n "${package_root}/packaging/ubuntu/uninstall-taskcaged.sh" --purge-config >/dev/null 2>&1 || true
  fi
  sudo -n userdel taskcage >/dev/null 2>&1 || true
  sudo -n groupdel taskcage >/dev/null 2>&1 || true
  if [[ -n "${resolved_test_root}" && -d "${resolved_test_root}" ]]; then
    case "${resolved_test_root}" in
      /tmp/taskcage-release-smoke.*) rm -rf -- "${resolved_test_root}" ;;
      *) echo "WARN: 예상하지 않은 임시 경로는 제거하지 않습니다: ${resolved_test_root}" >&2 ;;
    esac
  fi
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

tar --extract --gzip --no-same-owner --file "${archive_path}" -C "${test_root}"

for packaged_file in \
  bin/taskcaged \
  packaging/ubuntu/install-taskcaged.sh \
  packaging/ubuntu/uninstall-taskcaged.sh \
  packaging/ubuntu/taskcaged.env \
  packaging/ubuntu/taskcaged.service \
  README.md \
  LICENSE; do
  [[ -f "${package_root}/${packaged_file}" && ! -L "${package_root}/${packaged_file}" ]] || {
    echo "ERROR: packaged file is missing or unsafe: ${packaged_file}" >&2
    exit 1
  }
done
[[ -x "${package_root}/bin/taskcaged" ]]
bash -n "${package_root}/packaging/ubuntu/install-taskcaged.sh"
bash -n "${package_root}/packaging/ubuntu/uninstall-taskcaged.sh"

release_metadata_path="${test_root}/releases.json"
printf '[{"draft":false,"tag_name":"taskcaged-v0.0.1"},{"draft":true,"tag_name":"taskcaged-v99.0.0"},{"draft":false,"tag_name":"taskcaged-v%s"}]\n' \
  "${release_version}" >"${release_metadata_path}"

installation_attempted=true
sudo -n env TASKCAGE_RELEASE_API_URL="file://${release_metadata_path}" \
  bash "${bootstrap_installer_path}" \
  --release-url "file://$(dirname -- "${archive_path}")"
sudo -n systemd-analyze verify taskcaged.service

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
grep -q "\"daemonVersion\":\"${release_version}\"" <<<"${status_json}"

sudo -n "${package_root}/packaging/ubuntu/uninstall-taskcaged.sh" --purge-config
installation_attempted=false
[[ ! -e /usr/local/bin/taskcaged ]]
[[ ! -e /etc/systemd/system/taskcaged.service ]]
[[ ! -e /etc/taskcage/taskcaged.env ]]

trap - EXIT
cleanup
if id taskcage >/dev/null 2>&1 || getent group taskcage >/dev/null 2>&1; then
  echo "ERROR: release smoke cleanup left the taskcage account or group behind" >&2
  exit 1
fi
echo "PASS: v${release_version} daemon archive checksum, layout, 설치, readiness와 제거를 확인했습니다"
