#!/bin/sh
set -eu

mkdir -p /run/taskcage /taskcage-work /taskcage-work/artifacts
chmod 0700 /run/taskcage /taskcage-work /taskcage-work/artifacts

if [ "${1:-}" = "serve" ]; then
  taskcage-container-prepare-ffmpeg-package
  taskcage-container-prepare-ffmpeg-capsule
  if [ "${TASKCAGE_INSTALL_BENCHMARK_CAPSULES:-false}" = "true" ]; then
    taskcage-container-prepare-benchmark-capsules
  fi
  set -- "$@" \
    --bundle-cache-root /taskcage-work/runtime-package-cache
fi

exec /usr/local/bin/taskcaged "$@"
