#!/bin/sh
set -eu

jobs=/sys/fs/cgroup/jobs
attempt=0
while find "${jobs}" -mindepth 1 -maxdepth 1 -type d -print -quit | grep -q .; do
  attempt=$((attempt + 1))
  if [ "${attempt}" -ge 30 ]; then
    echo "FAIL: a TaskCage job cgroup remains after Java E2E" >&2
    exit 1
  fi
  sleep 1
done

echo "PASS: no TaskCage job cgroup remains after Java E2E"
