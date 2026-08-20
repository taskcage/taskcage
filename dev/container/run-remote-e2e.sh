#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
compose=(docker compose --file "${script_dir}/compose.yml" --profile remote-test)

cleanup() { "${compose[@]}" down --volumes --remove-orphans >/dev/null 2>&1 || true; }
trap cleanup EXIT

cleanup
"${compose[@]}" up --build --detach --wait remote-taskcaged
"${compose[@]}" run --build --rm java-remote-e2e
"${compose[@]}" exec --no-TTY remote-taskcaged taskcage-container-verify-cleanup
