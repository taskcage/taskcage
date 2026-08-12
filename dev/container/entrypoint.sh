#!/bin/sh
set -eu

mkdir -p /run/taskcage /taskcage-work /taskcage-work/artifacts
chmod 0700 /run/taskcage /taskcage-work /taskcage-work/artifacts

if [ "${1:-}" = "serve" ]; then
  ffmpeg_package_digest="$(taskcage-container-prepare-ffmpeg-package)"
  set -- "$@" \
    --runtime-package-cache-root /taskcage-work/runtime-package-cache \
    --ffmpeg-audio-to-wav-package-digest "${ffmpeg_package_digest}"
fi

exec /usr/local/bin/taskcaged "$@"
