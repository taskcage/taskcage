#!/bin/sh
set -eu

mkdir -p /run/taskcage /taskcage-work /taskcage-work/artifacts /etc/taskcage/trusted-capsules.d
chmod 0700 /run/taskcage /taskcage-work /taskcage-work/artifacts
chmod 0755 /etc/taskcage/trusted-capsules.d

if [ "${1:-}" = "serve" ]; then
  ffmpeg_package_digest="$(taskcage-container-prepare-ffmpeg-package)"
  taskcage-container-prepare-ffmpeg-bundle "${ffmpeg_package_digest}"
  install -m 0644 /taskcage-work/bundle-signing-key.pub \
    /etc/taskcage/trusted-capsules.d/container-test.pub
  set -- "$@" \
    --bundle-cache-root /taskcage-work/runtime-package-cache
fi

exec /usr/local/bin/taskcaged "$@"
