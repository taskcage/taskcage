#!/bin/sh
set -u

"$@" &
child_pid=$!
wait "${child_pid}"
exit_code=$?

memory_peak=$(cat /sys/fs/cgroup/memory.peak 2>/dev/null || echo -1)
cpu_usage=$(awk '$1 == "usage_usec" { print $2 }' /sys/fs/cgroup/cpu.stat 2>/dev/null || echo -1)
printf 'TASKCAGE_DOCKER_TASK_METRICS %s %s %s\n' "${memory_peak}" "${cpu_usage}" "${exit_code}"
exit "${exit_code}"
