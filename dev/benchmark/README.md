# Local execution-worker benchmark PoC

This is a manual development experiment. It compares a Java execution worker that starts external programs through
`ProcessBuilder` with a `taskcaged` execution worker that starts the same programs through Local UDS. It is not a
release benchmark or a CI performance gate.

The benchmark intentionally excludes its request driver from the execution-worker comparison:

```text
ProcessBuilder: Java worker container -> FFmpeg / fixture
TaskCage:       taskcaged container -> cgroup Task -> FFmpeg / fixture
```

The TaskCage driver is a separate Java container. Its memory is reported only inside `workerResult` and must not be
added to the daemon footprint. The TaskCage result reports each task cgroup's peak memory; the wrapper adds the
daemon container's `memory.peak` separately.

## Run

Run from the repository root on a trusted Docker environment with Linux cgroup v2 support:

```bash
bash dev/benchmark/run-local.sh
```

The default concurrency is two. Override it only on a development machine with enough CPU and memory:

```bash
BENCHMARK_CONCURRENCY=4 bash dev/benchmark/run-local.sh
```

To focus on concurrent normal work without running the cleanup and memory scenarios:

```bash
BENCHMARK_CONCURRENCY=16 BENCHMARK_SCENARIOS=normal bash dev/benchmark/run-local.sh
```

The script sets the benchmark daemon's task capacity to the requested concurrency unless
`BENCHMARK_MAX_CONCURRENT_TASKS` is explicitly supplied. This avoids measuring admission rejection as execution time.

The script writes one JSON document under `dev/benchmark/results/` and removes containers and volumes when it exits.

## Scenarios

- `normal`: concurrent one-second FFmpeg sine-to-WAVE conversions.
- `timeout_child`: one normal conversion plus `ghost-tree`; the Java worker force-stops only the root process while
  TaskCage applies a one-second wall-time cgroup limit.
- `memory_limit`: one normal conversion plus a 64 MiB memory fixture; TaskCage gives that fixture a 16 MiB task limit.

The ProcessBuilder memory case is deliberately cancelled by the benchmark after one second. It has no equivalent
per-process memory boundary, so this is a stability contrast rather than an equal-limit throughput comparison.

## Interpretation

Docker Desktop results are directional only. Do not use them for public performance claims. Compare normal latency
and container memory separately from cleanup behavior. In particular, a TaskCage task result with a terminal state
means its cgroup cleanup was confirmed; `taskcage-container-verify-cleanup` verifies no TaskCage job cgroup remains.
