#!/bin/sh
set -eu

wait_for_empty_directory() {
  directory=$1
  residue=$2
  attempt=0

  [ -d "${directory}" ] || return 0
  while find "${directory}" -mindepth 1 -maxdepth 1 -print -quit | grep -q .; do
    attempt=$((attempt + 1))
    if [ "${attempt}" -ge 30 ]; then
      echo "FAIL: ${residue} remains after Java E2E: ${directory}" >&2
      find "${directory}" -mindepth 1 -maxdepth 1 -print >&2
      exit 1
    fi
    sleep 1
  done
}

artifact_root=${TASKCAGE_ARTIFACT_ROOT:-/taskcage-work/artifacts}

[ -d /sys/fs/cgroup/jobs ] || {
  echo "FAIL: TaskCage job cgroup root is missing: /sys/fs/cgroup/jobs" >&2
  exit 1
}
[ -d "${artifact_root}" ] || {
  echo "FAIL: TaskCage Artifact root is missing: ${artifact_root}" >&2
  exit 1
}

wait_for_empty_directory /sys/fs/cgroup/jobs "a TaskCage job cgroup"
wait_for_empty_directory "${artifact_root}/.taskcage/staging" "a Local Artifact staging entry"
wait_for_empty_directory "${artifact_root}/staging" "a Remote upload staging entry"
wait_for_empty_directory "${artifact_root}/completed-inputs" "an unconsumed Remote input"
wait_for_empty_directory "${artifact_root}/task-inputs" "a task-owned Remote input"

if [ -d "${artifact_root}/outputs" ]; then
  temporary_output=$(find "${artifact_root}/outputs" -mindepth 1 -maxdepth 1 -name '.*.tmp' -print -quit)
  if [ -n "${temporary_output}" ]; then
    echo "FAIL: a temporary Remote output remains after Java E2E: ${temporary_output}" >&2
    exit 1
  fi
fi

echo "PASS: no TaskCage job cgroup or Artifact staging residue remains after Java E2E"
