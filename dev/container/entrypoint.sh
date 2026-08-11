#!/bin/sh
set -eu

mkdir -p /run/taskcage /taskcage-work
chmod 0700 /run/taskcage /taskcage-work

exec /usr/local/bin/taskcaged "$@"
