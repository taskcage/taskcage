#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
compose=(docker compose --file "${script_dir}/compose.yml")

cleanup() {
  "${compose[@]}" down --volumes --remove-orphans >/dev/null 2>&1 || true
}
trap cleanup EXIT

"${compose[@]}" up --build --detach --wait taskcaged
"${compose[@]}" --profile example run --build --rm ffmpeg-example
"${compose[@]}" exec --no-TTY taskcaged taskcage-container-verify-cleanup
