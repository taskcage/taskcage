#!/bin/sh
set -eu

source_root=/taskcage-work/runtime-package-source
cache_root=/taskcage-work/runtime-package-cache
pack=/taskcage-work/ffmpeg-audio-to-wav-1.0.0.tccapsule
capsulefile=/usr/local/share/taskcage/ffmpeg-audio-to-wav.Capsulefile

case "$(uname -m)" in
  x86_64) platform=linux/amd64 ;;
  aarch64) platform=linux/arm64 ;;
  *)
    echo "ERROR: Capsule Pack requires Linux x86_64 or aarch64" >&2
    exit 1
    ;;
esac

rm -f -- "${pack}"
mkdir -p "${cache_root}"
chmod 0700 "${cache_root}"
taskcage capsule build "${capsulefile}" \
  --runtime-package "${source_root}" \
  --platform "${platform}" \
  --output "${pack}" >/dev/null
taskcage capsule install "${pack}" --cache-root "${cache_root}" >/dev/null
