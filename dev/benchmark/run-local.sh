#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
compose=(docker compose --file "${script_dir}/compose.yml" --profile benchmark)
result_dir="${script_dir}/results"
timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
result_file="${result_dir}/local-${timestamp}.json"
concurrency="${BENCHMARK_CONCURRENCY:-2}"

cleanup() {
  "${compose[@]}" down --volumes --remove-orphans >/dev/null 2>&1 || true
}
trap cleanup EXIT

mkdir -p "${result_dir}"

"${compose[@]}" build --quiet taskcaged benchmark-worker >&2

run_mode() {
  local mode="$1"
  local scenario="$2"

  "${compose[@]}" down --volumes --remove-orphans >/dev/null 2>&1 || true
  if [[ "${mode}" == "taskcage" ]]; then
    "${compose[@]}" up --detach --wait taskcaged >&2
  fi

  local result
  result="$("${compose[@]}" run --rm --no-deps \
    -e "BENCHMARK_MODE=${mode}" \
    -e "BENCHMARK_SCENARIO=${scenario}" \
    -e "BENCHMARK_CONCURRENCY=${concurrency}" \
    benchmark-worker | tail -n 1)"

  if [[ "${mode}" == "taskcage" ]]; then
    local daemon_peak cleanup_verified
    daemon_peak="$("${compose[@]}" exec --no-TTY taskcaged cat /sys/fs/cgroup/memory.peak)"
    "${compose[@]}" exec --no-TTY taskcaged taskcage-container-verify-cleanup >&2
    cleanup_verified=true
    printf '{"workerResult":%s,"daemonContainerMemoryPeakBytes":%s,"daemonCleanupVerified":%s}' \
      "${result}" "${daemon_peak}" "${cleanup_verified}"
  else
    printf '%s' "${result}"
  fi
}

printf '{\n  "environment":{"kind":"local-docker-poc","concurrency":%s},\n  "scenarios":[' "${concurrency}" >"${result_file}"
separator=""
for scenario in normal timeout_child memory_limit; do
  process_builder="$(run_mode processbuilder "${scenario}")"
  taskcage="$(run_mode taskcage "${scenario}")"
  printf '%s\n    {"name":"%s","processBuilder":%s,"taskCage":%s}' \
    "${separator}" "${scenario}" "${process_builder}" "${taskcage}" >>"${result_file}"
  separator=','
done
printf '\n  ]\n}\n' >>"${result_file}"

cat "${result_file}"
