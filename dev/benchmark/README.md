# Local execution-worker benchmark PoC

This is a manual development experiment. It compares direct Java ProcessBuilder execution with TaskCage for the same
trusted CLI. It is
not a release benchmark or a CI performance gate.

The benchmark intentionally excludes its request driver from the execution-worker comparison:

```text
ProcessBuilder: Java worker container -> FFmpeg / fixture
TaskCage:       Java worker -> taskcaged container -> cgroup Task -> FFmpeg / fixture
```

The worker driver is reported separately from the TaskCage daemon. TaskCage also returns each Task cgroup's peak
memory and CPU time.

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

For a local repeatability check, warm up five batches and measure 30 more:

```bash
BENCHMARK_CONCURRENCY=16 BENCHMARK_SCENARIOS=normal \
  BENCHMARK_WARMUP=5 BENCHMARK_ITERATIONS=30 bash dev/benchmark/run-local.sh
```

The output reports batch and individual-task p50/p95 latency, result validation, terminal reasons, cleanup evidence,
and the available CPU/memory measurements. It writes both JSON and a local HTML report under
`dev/benchmark/results/`.

The script writes one JSON document under `dev/benchmark/results/` and removes containers and volumes when it exits.

## Scenarios

- `normal`: concurrent two-second FFmpeg sine-to-WAVE conversions with a non-empty output check.
- `timeout_child`: one normal conversion plus `ghost-tree`; the Java worker force-stops only the root process while
  TaskCage applies a one-second wall-time cgroup limit.
- `memory_limit`: one normal conversion plus a 64 MiB memory fixture; TaskCage gives that fixture a 16 MiB task limit.
- `mixed_failure`: 80% normal FFmpeg work, 10% timeout child trees and 10% memory-pressure fixtures. The report
  separates normal-task p50/p95 from the failure outcomes.

The ProcessBuilder memory case is deliberately cancelled by the benchmark after one second. It has no equivalent
per-process memory boundary, so this is a stability contrast rather than an equal-limit throughput comparison.

## Measurement boundary

Each latency starts immediately before the worker submits its execution request and ends only after the command has
finished and the expected normal output is checked. Therefore Local UDS/SDK overhead is included for TaskCage. Image
build/pull, JVM/container/daemon startup and the configured warm-up batches are excluded.

## Interpretation

Docker Desktop results are directional only. Do not use them for public performance claims. The main evidence is
cleanup correctness and blast-radius containment, not a universal speed claim. A TaskCage terminal task result means
its cgroup cleanup was confirmed; `taskcage-container-verify-cleanup` verifies no TaskCage job cgroup remains.

## Evidence-sized local run

The following is suitable for a local evidence pass, not CI. It produces 240 tasks per execution mode: 192 normal
FFmpeg conversions and 48 controlled failures.

```bash
BENCHMARK_CONCURRENCY=8 BENCHMARK_SCENARIOS=mixed_failure \
  BENCHMARK_WARMUP=2 BENCHMARK_ITERATIONS=30 bash dev/benchmark/run-local.sh
```

Run the normal scenario separately at concurrency 1, 4 and 16 to compare normal-task latency. Use a native Linux
VM or host before making any public memory-limit or throughput claim.
