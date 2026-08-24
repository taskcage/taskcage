#!/bin/sh
set -eu

mkdir -p /run/taskcage \
  /var/lib/taskcage/artifacts \
  /var/lib/taskcage/runtime-package-cache
chmod 0700 /run/taskcage /var/lib/taskcage /var/lib/taskcage/artifacts \
  /var/lib/taskcage/runtime-package-cache

exec "$@"
