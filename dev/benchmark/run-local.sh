#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
compose=(docker compose --file "${script_dir}/compose.yml" --profile benchmark)
result_dir="${script_dir}/results"
timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
result_file="${result_dir}/local-${timestamp}.json"
report_file="${result_dir}/report-local-${timestamp}.html"
concurrency="${BENCHMARK_CONCURRENCY:-2}"
max_concurrent_tasks="${BENCHMARK_MAX_CONCURRENT_TASKS:-${concurrency}}"
scenarios="${BENCHMARK_SCENARIOS:-normal timeout_child memory_limit}"
warmup_batches="${BENCHMARK_WARMUP:-0}"
measured_batches="${BENCHMARK_ITERATIONS:-1}"

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
    BENCHMARK_MAX_CONCURRENT_TASKS="${max_concurrent_tasks}" "${compose[@]}" up --detach --wait taskcaged >&2
  fi

  local result
  if ! result="$("${compose[@]}" run --rm --no-deps \
    -e "BENCHMARK_MODE=${mode}" \
    -e "BENCHMARK_SCENARIO=${scenario}" \
    -e "BENCHMARK_CONCURRENCY=${concurrency}" \
    -e "BENCHMARK_WARMUP=${warmup_batches}" \
    -e "BENCHMARK_ITERATIONS=${measured_batches}" \
    benchmark-worker)"; then
    echo "ERROR: ${mode} runner failed for ${scenario}" >&2
    return 1
  fi
  if [[ "${result}" != \{*\} ]]; then
    echo "ERROR: ${mode} runner did not return a JSON object for ${scenario}" >&2
    return 1
  fi

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

printf '{\n  "environment":{"kind":"local-docker-poc","concurrency":%s,"warmupBatches":%s,"measuredBatches":%s},\n  "scenarios":[' \
  "${concurrency}" "${warmup_batches}" "${measured_batches}" >"${result_file}"
separator=""
for scenario in ${scenarios}; do
  if ! process_builder="$(run_mode processbuilder "${scenario}")"; then exit 1; fi
  if ! taskcage="$(run_mode taskcage "${scenario}")"; then exit 1; fi
  printf '%s\n    {"name":"%s","processBuilder":%s,"taskCage":%s}' \
    "${separator}" "${scenario}" "${process_builder}" "${taskcage}" >>"${result_file}"
  separator=','
done
printf '\n  ]\n}\n' >>"${result_file}"

python3 "${script_dir}/render-report.py" "${result_file}" "${report_file}"
printf 'JSON: %s\nHTML: %s\n' "${result_file}" "${report_file}"
cat "${result_file}"
