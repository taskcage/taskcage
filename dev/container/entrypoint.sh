#!/bin/sh
set -eu

mkdir -p /run/taskcage /taskcage-work /taskcage-work/artifacts
chmod 0700 /run/taskcage /taskcage-work /taskcage-work/artifacts

if [ "${1:-}" = "serve" ]; then
  ffmpeg_package_digest="$(taskcage-container-prepare-ffmpeg-package)"
  taskcage-container-prepare-ffmpeg-bundle "${ffmpeg_package_digest}"
  set -- "$@" \
    --bundle-cache-root /taskcage-work/runtime-package-cache
fi

exec /usr/local/bin/taskcaged "$@"
