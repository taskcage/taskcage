#!/usr/bin/env bash
set -euo pipefail

# GitHub-hosted Ubuntu images may point apt at a regional Azure mirror. During a
# mirror incident apt can spend the whole job timeout retrying an unreachable
# endpoint. Use the official global archive for this CI-only dependency install.
readonly ubuntu_sources=(
  /etc/apt/sources.list.d/ubuntu.sources
  /etc/apt/sources.list
)
readonly stable_mirror="https://archive.ubuntu.com/ubuntu"

source_updated=false
for source_file in "${ubuntu_sources[@]}"; do
  if [[ -f "${source_file}" ]] && grep -Eq 'mirror\+file:/etc/apt/apt-mirrors\.txt|azure\.archive\.ubuntu\.com' "${source_file}"; then
    sed -i \
      -e "s|mirror+file:/etc/apt/apt-mirrors.txt|${stable_mirror}|g" \
      -e "s|http://azure.archive.ubuntu.com/ubuntu|${stable_mirror}|g" \
      -e "s|https://azure.archive.ubuntu.com/ubuntu|${stable_mirror}|g" \
      "${source_file}"
    source_updated=true
  fi
done

if [[ "${source_updated}" != true ]]; then
  echo "No Azure Ubuntu mirror source was found; using the runner's configured sources." >&2
fi

timeout 180s apt-get \
  -o Acquire::Retries=3 \
  -o Acquire::http::Timeout=30 \
  -o Acquire::https::Timeout=30 \
  update

timeout 120s apt-get install -y --no-install-recommends ffmpeg
