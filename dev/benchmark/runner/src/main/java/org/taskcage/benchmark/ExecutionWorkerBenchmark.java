package org.taskcage.benchmark;

import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Duration;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.UUID;
import java.util.concurrent.Callable;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.Future;
import java.util.concurrent.TimeUnit;
import org.taskcage.sdk.CpuQuota;
import org.taskcage.sdk.ExecutionResult;
import org.taskcage.sdk.ExternalCommand;
import org.taskcage.sdk.FinishedTaskSnapshot;
import org.taskcage.sdk.ResourceBudget;
import org.taskcage.sdk.TaskCageClient;
import org.taskcage.sdk.TaskCageClientConfig;
import org.taskcage.sdk.TaskSpec;

/**
 * Manual, local-only comparison of a Java ProcessBuilder execution worker and taskcaged.
 * The load generator is intentionally outside this process; this class measures its execution container only.
 */
public final class ExecutionWorkerBenchmark {
    private static final Path WORK_ROOT = Path.of("/taskcage-work/benchmark");
    private static final Path FFMPEG = Path.of("/usr/bin/ffmpeg");
    private static final Path GHOST_TREE = Path.of("/usr/local/libexec/taskcage/ghost-tree");
    private static final Path MEMORY_HOG = Path.of("/usr/local/libexec/taskcage/memory-hog");

    private ExecutionWorkerBenchmark() {}

    public static void main(String[] args) throws Exception {
        Mode mode = Mode.parse(requiredEnv("BENCHMARK_MODE"));
        Scenario scenario = Scenario.parse(requiredEnv("BENCHMARK_SCENARIO"));
        int concurrency = positiveInt(System.getenv().getOrDefault("BENCHMARK_CONCURRENCY", "2"));
        Path workDirectory = WORK_ROOT.resolve(mode.jsonName + "-" + scenario.jsonName + "-" + UUID.randomUUID());
        Files.createDirectories(workDirectory);

        long startedNanos = System.nanoTime();
        List<TaskMetric> tasks = run(mode, scenario, concurrency, workDirectory);
        long totalMillis = elapsedMillis(startedNanos);
        System.out.println(render(mode, scenario, concurrency, totalMillis, tasks));
    }

    private static List<TaskMetric> run(Mode mode, Scenario scenario, int concurrency, Path workDirectory)
            throws InterruptedException, ExecutionException {
        ExecutorService pool = Executors.newFixedThreadPool(concurrency);
        try {
            List<Future<TaskMetric>> futures = new ArrayList<>();
            for (int index = 0; index < concurrency; index++) {
                int taskIndex = index;
                Callable<TaskMetric> task = mode == Mode.PROCESS_BUILDER
                        ? () -> runProcessBuilder(scenario, taskIndex, workDirectory)
                        : () -> runTaskCage(scenario, taskIndex, workDirectory);
                futures.add(pool.submit(task));
            }
            List<TaskMetric> results = new ArrayList<>();
            for (Future<TaskMetric> future : futures) results.add(future.get());
            return results;
        } finally {
            pool.shutdownNow();
            pool.awaitTermination(5, TimeUnit.SECONDS);
        }
    }

    private static TaskMetric runProcessBuilder(Scenario scenario, int index, Path workDirectory) throws Exception {
        Command command = commandFor(scenario, index, workDirectory);
        long startedNanos = System.nanoTime();
        List<String> processArguments = new ArrayList<>();
        processArguments.add(command.program.toString());
        processArguments.addAll(command.arguments);
        Process process = new ProcessBuilder(processArguments)
                .directory(workDirectory.toFile())
                .redirectOutput(ProcessBuilder.Redirect.DISCARD)
                .redirectError(ProcessBuilder.Redirect.DISCARD)
                .start();

        List<ProcessHandle> descendants = List.of();
        String termination;
        if (scenario == Scenario.TIMEOUT_CHILD && command.readyPath != null) {
            awaitReady(command.readyPath, Duration.ofSeconds(3));
            descendants = process.toHandle().descendants().toList();
            process.destroyForcibly();
            process.waitFor(5, TimeUnit.SECONDS);
            termination = "TIMED_OUT_ROOT_ONLY";
        } else if (scenario == Scenario.MEMORY_LIMIT && index > 0) {
            Thread.sleep(1_000);
            process.destroyForcibly();
            process.waitFor(5, TimeUnit.SECONDS);
            termination = "CANCELLED_BY_BENCHMARK";
        } else {
            process.waitFor(20, TimeUnit.SECONDS);
            termination = process.exitValue() == 0 ? "EXITED" : "EXITED_NON_ZERO";
        }

        int residual = alive(descendants);
        descendants.forEach(handle -> {
            if (handle.isAlive()) handle.destroyForcibly();
        });
        return new TaskMetric(elapsedMillis(startedNanos), termination, null, residual, true);
    }

    private static TaskMetric runTaskCage(Scenario scenario, int index, Path workDirectory) throws Exception {
        Command command = commandFor(scenario, index, workDirectory);
        long startedNanos = System.nanoTime();
        ResourceBudget budget = budgetFor(scenario, index);
        try (TaskCageClient client = TaskCageClient.connect(TaskCageClientConfig.builder()
                .socketPath(Path.of(requiredEnv("TASKCAGE_SOCKET")))
                .connectTimeout(Duration.ofSeconds(2))
                .requestTimeout(Duration.ofSeconds(5))
                .build())) {
            FinishedTaskSnapshot finished = client.run(UUID.randomUUID(), new TaskSpec(
                    new ExternalCommand(command.program, command.arguments, workDirectory, Map.of()), budget),
                    Duration.ofSeconds(25));
            ExecutionResult result = finished.result();
            return new TaskMetric(elapsedMillis(startedNanos), result.terminationReason().name(),
                    result.usage().memoryPeakBytes(), 0, true);
        }
    }

    private static ResourceBudget budgetFor(Scenario scenario, int index) {
        long memory = scenario == Scenario.MEMORY_LIMIT && index > 0 ? 16L * 1024 * 1024 : 128L * 1024 * 1024;
        Duration wallTime = scenario == Scenario.TIMEOUT_CHILD && index > 0 ? Duration.ofSeconds(1) : Duration.ofSeconds(15);
        return new ResourceBudget(new CpuQuota(100_000, 100_000), memory, 32, wallTime, 4_096, 4_096);
    }

    private static Command commandFor(Scenario scenario, int index, Path workDirectory) {
        if (scenario == Scenario.TIMEOUT_CHILD && index > 0) {
            Path ready = workDirectory.resolve("ghost-" + index + ".ready");
            return new Command(GHOST_TREE, List.of("--hold-parent", ready.toString()), ready);
        }
        if (scenario == Scenario.MEMORY_LIMIT && index > 0) {
            return new Command(MEMORY_HOG, List.of("67108864", "30"), null);
        }
        Path output = workDirectory.resolve("normal-" + index + ".wav");
        return new Command(FFMPEG, List.of(
                "-hide_banner", "-loglevel", "error", "-nostdin", "-y",
                "-f", "lavfi", "-i", "sine=frequency=1000:duration=1",
                "-c:a", "pcm_s16le", output.toString()), null);
    }

    private static void awaitReady(Path ready, Duration timeout) throws Exception {
        long deadline = System.nanoTime() + timeout.toNanos();
        while (System.nanoTime() < deadline) {
            if (Files.exists(ready)) return;
            Thread.sleep(25);
        }
        throw new IllegalStateException("ghost-tree did not become ready");
    }

    private static int alive(List<ProcessHandle> handles) {
        try { Thread.sleep(200); } catch (InterruptedException exception) { Thread.currentThread().interrupt(); }
        return (int) handles.stream().filter(ProcessHandle::isAlive).count();
    }

    private static long elapsedMillis(long startedNanos) {
        return TimeUnit.NANOSECONDS.toMillis(System.nanoTime() - startedNanos);
    }

    private static String render(Mode mode, Scenario scenario, int concurrency, long totalMillis, List<TaskMetric> tasks) {
        List<Long> latencies = tasks.stream().map(TaskMetric::latencyMillis).sorted().toList();
        Map<String, Integer> reasons = new LinkedHashMap<>();
        for (TaskMetric task : tasks) reasons.merge(task.termination, 1, Integer::sum);
        long taskPeak = tasks.stream().map(TaskMetric::taskMemoryPeakBytes).filter(value -> value != null)
                .mapToLong(Long::longValue).max().orElse(0);
        int residual = tasks.stream().mapToInt(TaskMetric::residualProcesses).sum();
        return "{"
                + "\"mode\":\"" + mode.jsonName + "\","
                + "\"scenario\":\"" + scenario.jsonName + "\","
                + "\"concurrency\":" + concurrency + ","
                + "\"latencyMs\":{\"total\":" + totalMillis + ",\"p50\":" + percentile(latencies, 0.50)
                + ",\"p95\":" + percentile(latencies, 0.95) + "},"
                + "\"tasks\":{\"submitted\":" + tasks.size() + ",\"terminationReasons\":" + reasonsJson(reasons) + "},"
                + "\"taskMemoryPeakBytes\":" + taskPeak + ","
                + "\"executorContainerMemoryPeakBytes\":" + cgroupMemoryPeak() + ","
                + "\"cleanup\":{\"residualProcesses\":" + residual
                + ",\"cleanupConfirmed\":" + tasks.stream().allMatch(TaskMetric::cleanupConfirmed) + "}"
                + "}";
    }

    private static long percentile(List<Long> values, double percentile) {
        if (values.isEmpty()) return 0;
        return values.get((int) Math.ceil(percentile * values.size()) - 1);
    }

    private static String reasonsJson(Map<String, Integer> reasons) {
        return reasons.entrySet().stream().map(entry -> "\"" + entry.getKey() + "\":" + entry.getValue())
                .collect(java.util.stream.Collectors.joining(",", "{", "}"));
    }

    private static long cgroupMemoryPeak() {
        try { return Long.parseLong(Files.readString(Path.of("/sys/fs/cgroup/memory.peak")).trim()); }
        catch (IOException | NumberFormatException ignored) { return -1; }
    }

    private static String requiredEnv(String name) {
        String value = System.getenv(name);
        if (value == null || value.isBlank()) throw new IllegalArgumentException(name + " is required");
        return value;
    }

    private static int positiveInt(String value) {
        int parsed = Integer.parseInt(value);
        if (parsed < 1) throw new IllegalArgumentException("BENCHMARK_CONCURRENCY must be positive");
        return parsed;
    }

    private enum Mode {
        PROCESS_BUILDER("processbuilder"), TASK_CAGE("taskcage");
        private final String jsonName;
        Mode(String jsonName) { this.jsonName = jsonName; }
        static Mode parse(String value) { return switch (value) {
            case "processbuilder" -> PROCESS_BUILDER;
            case "taskcage" -> TASK_CAGE;
            default -> throw new IllegalArgumentException("BENCHMARK_MODE must be processbuilder or taskcage");
        }; }
    }

    private enum Scenario {
        NORMAL("normal"), TIMEOUT_CHILD("timeout_child"), MEMORY_LIMIT("memory_limit");
        private final String jsonName;
        Scenario(String jsonName) { this.jsonName = jsonName; }
        static Scenario parse(String value) { return switch (value) {
            case "normal" -> NORMAL;
            case "timeout_child" -> TIMEOUT_CHILD;
            case "memory_limit" -> MEMORY_LIMIT;
            default -> throw new IllegalArgumentException("unknown BENCHMARK_SCENARIO: " + value);
        }; }
    }

    private record Command(Path program, List<String> arguments, Path readyPath) {}
    private record TaskMetric(long latencyMillis, String termination, Long taskMemoryPeakBytes,
                              int residualProcesses, boolean cleanupConfirmed) {}
}
