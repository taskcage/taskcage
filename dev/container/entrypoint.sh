#!/bin/sh
set -eu

mkdir -p /run/taskcage /taskcage-work /taskcage-work/artifacts
chmod 0700 /run/taskcage /taskcage-work /taskcage-work/artifacts

exec /usr/local/bin/taskcaged "$@"
