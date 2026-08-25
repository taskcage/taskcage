package org.taskcage.benchmark;

import java.io.IOException;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.MessageDigest;
import java.time.Duration;
import java.util.ArrayList;
import java.util.HexFormat;
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
import org.taskcage.sdk.ArtifactPath;
import org.taskcage.sdk.CapsuleExecutionResult;
import org.taskcage.sdk.CapsuleIdentity;
import org.taskcage.sdk.CapsuleRequest;
import org.taskcage.sdk.CapsuleRunner;
import org.taskcage.sdk.ExecutionResult;
import org.taskcage.sdk.LocalInputArtifact;
import org.taskcage.sdk.ProfileResourceOverrides;
import org.taskcage.sdk.PublishedArtifact;
import org.taskcage.sdk.Sha256Digest;
import org.taskcage.sdk.TaskCageClient;
import org.taskcage.sdk.TaskCageClientConfig;

/**
 * Manual, local-only comparison of a Java ProcessBuilder execution worker and taskcaged.
 * The load generator is intentionally outside this process; this class measures its execution container only.
 */
public final class ExecutionWorkerBenchmark {
    private static final Path WORK_ROOT = Path.of("/taskcage-work/benchmark");
    private static final Path FFMPEG = Path.of("/usr/bin/ffmpeg");
    private static final Path GHOST_TREE = Path.of("/usr/local/libexec/taskcage/ghost-tree");
    private static final Path MEMORY_HOG = Path.of("/usr/local/libexec/taskcage/memory-hog");
    private static final CapsuleIdentity FFMPEG_CAPSULE =
            new CapsuleIdentity("ffmpeg-audio-to-wav", "1.0.0");
    private static final CapsuleIdentity GHOST_TREE_CAPSULE =
            new CapsuleIdentity("ghost-tree-timeout", "1.0.0");
    private static final CapsuleIdentity MEMORY_HOG_CAPSULE =
            new CapsuleIdentity("memory-hog-limit", "1.0.0");

    private ExecutionWorkerBenchmark() {}

    public static void main(String[] args) throws Exception {
        Mode mode = Mode.parse(requiredEnv("BENCHMARK_MODE"));
        Scenario scenario = Scenario.parse(requiredEnv("BENCHMARK_SCENARIO"));
        int concurrency = positiveInt(System.getenv().getOrDefault("BENCHMARK_CONCURRENCY", "2"));
        int warmupBatches = nonNegativeInt(System.getenv().getOrDefault("BENCHMARK_WARMUP", "0"));
        int measuredBatches = positiveInt(System.getenv().getOrDefault("BENCHMARK_ITERATIONS", "1"));

        List<BatchMetric> measured = new ArrayList<>();
        for (int batch = 0; batch < warmupBatches + measuredBatches; batch++) {
            Path workDirectory = WORK_ROOT.resolve(mode.jsonName + "-" + scenario.jsonName + "-" + UUID.randomUUID());
            Files.createDirectories(workDirectory);
            InputArtifact source = createInputArtifact();
            try {
                long startedNanos = System.nanoTime();
                List<TaskMetric> tasks = run(mode, scenario, batch, concurrency, workDirectory, source);
                if (batch >= warmupBatches) measured.add(new BatchMetric(elapsedMillis(startedNanos), tasks));
            } finally {
                source.delete();
            }
        }
        List<TaskMetric> measuredTasks = measured.stream().flatMap(batch -> batch.tasks.stream()).toList();
        List<String> validationErrors = validate(mode, scenario, measuredTasks);
        System.out.println(render(mode, scenario, concurrency, warmupBatches, measured, validationErrors));
        if (!validationErrors.isEmpty()) {
            System.err.println("benchmark intent validation failed:\n - " + String.join("\n - ", validationErrors));
            System.exit(2);
        }
    }

    private static List<TaskMetric> run(Mode mode, Scenario scenario, int batch, int concurrency, Path workDirectory,
                                        InputArtifact source)
            throws InterruptedException, ExecutionException {
        ExecutorService pool = Executors.newFixedThreadPool(concurrency);
        try {
            List<Future<TaskMetric>> futures = new ArrayList<>();
            for (int index = 0; index < concurrency; index++) {
                int taskIndex = index;
                Callable<TaskMetric> task = switch (mode) {
                    case PROCESS_BUILDER -> () -> runProcessBuilder(
                            scenario, batch, taskIndex, concurrency, workDirectory, source);
                    case TASK_CAGE -> () -> runTaskCage(scenario, batch, taskIndex, concurrency, workDirectory, source);
                };
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

    private static TaskMetric runProcessBuilder(Scenario scenario, int batch, int index, int concurrency,
                                                Path workDirectory, InputArtifact source) throws Exception {
        Scenario taskScenario = taskScenario(scenario, batch, index, concurrency);
        Command command = commandFor(taskScenario, index, workDirectory, source.file());
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
        if (taskScenario == Scenario.TIMEOUT_CHILD && command.readyPath != null) {
            awaitReady(command.readyPath, Duration.ofSeconds(3));
            descendants = process.toHandle().descendants().toList();
            process.destroyForcibly();
            process.waitFor(5, TimeUnit.SECONDS);
            termination = "TIMED_OUT_ROOT_ONLY";
        } else if (taskScenario == Scenario.MEMORY_LIMIT) {
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
        return new TaskMetric(elapsedMillis(startedNanos), taskScenario.jsonName, termination, null, null,
                residual, residual == 0, taskScenario != Scenario.NORMAL || verifiedOutput(command));
    }

    private static TaskMetric runTaskCage(Scenario scenario, int batch, int index, int concurrency,
                                          Path workDirectory, InputArtifact source) throws Exception {
        Scenario taskScenario = taskScenario(scenario, batch, index, concurrency);
        long startedNanos = System.nanoTime();
        try (TaskCageClient client = TaskCageClient.connect(TaskCageClientConfig.builder()
                .socketPath(Path.of(requiredEnv("TASKCAGE_SOCKET")))
                .connectTimeout(Duration.ofSeconds(2))
                .requestTimeout(Duration.ofSeconds(5))
                .build());
             CapsuleRunner runner = CapsuleRunner.external(client)) {
            CapsuleExecutionResult finished = runner.execute(
                    UUID.randomUUID(), requestFor(taskScenario, source.reference()), Duration.ofSeconds(25));
            ExecutionResult result = finished.execution();
            boolean outputVerified = taskScenario != Scenario.NORMAL || verifiedCapsuleOutput(finished);
            return new TaskMetric(elapsedMillis(startedNanos), taskScenario.jsonName, result.terminationReason().name(),
                    result.usage().cpuTimeMicros(), result.usage().memoryPeakBytes(), 0, true,
                    outputVerified);
        }
    }

    private static CapsuleRequest requestFor(Scenario scenario, LocalInputArtifact source) {
        return switch (scenario) {
            case NORMAL -> CapsuleRequest.builder(FFMPEG_CAPSULE)
                    .artifact("source", source)
                    .int64("sample_rate_hz", 16_000)
                    .int64("channels", 1)
                    .build();
            case TIMEOUT_CHILD -> CapsuleRequest.builder(GHOST_TREE_CAPSULE)
                    .artifact("source", source)
                    .int64("marker", 1)
                    .resourceOverrides(ProfileResourceOverrides.builder()
                            .wallTimeLimit(Duration.ofSeconds(1))
                            .build())
                    .build();
            case MEMORY_LIMIT -> CapsuleRequest.builder(MEMORY_HOG_CAPSULE)
                    .artifact("source", source)
                    .int64("bytes", 67_108_864)
                    .int64("seconds", 30)
                    .build();
            case MIXED_FAILURE -> throw new IllegalArgumentException("mixed_failure is not a task workload");
        };
    }

    private static Command commandFor(Scenario scenario, int index, Path workDirectory, Path source) {
        if (scenario == Scenario.TIMEOUT_CHILD) {
            Path ready = workDirectory.resolve("ghost-" + index + ".ready");
            return new Command(GHOST_TREE, List.of("--hold-parent", ready.toString()), ready);
        }
        if (scenario == Scenario.MEMORY_LIMIT) {
            return new Command(MEMORY_HOG, List.of("67108864", "30"), null);
        }
        Path output = workDirectory.resolve("normal-" + index + ".wav");
        return new Command(FFMPEG, List.of(
                "-hide_banner", "-loglevel", "error", "-nostdin", "-y",
                "-i", source.toString(), "-map", "0:a:0", "-vn", "-c:a", "pcm_s16le",
                "-ar", "16000", "-ac", "1", output.toString()), null, output);
    }

    private static InputArtifact createInputArtifact() throws Exception {
        String directory = "benchmark-inputs/" + UUID.randomUUID();
        ArtifactPath path = new ArtifactPath(directory + "/source.wav");
        byte[] bytes = wave();
        Path file = artifactRoot().resolve(path.value());
        Files.createDirectories(file.getParent());
        Files.write(file, bytes);
        return new InputArtifact(file, new LocalInputArtifact(path, digest(bytes), bytes.length));
    }

    private static byte[] wave() {
        int samples = 8_000 * 2;
        ByteBuffer buffer = ByteBuffer.allocate(44 + samples * 2).order(ByteOrder.LITTLE_ENDIAN);
        buffer.put("RIFF".getBytes(StandardCharsets.US_ASCII));
        buffer.putInt(36 + samples * 2);
        buffer.put("WAVEfmt ".getBytes(StandardCharsets.US_ASCII));
        buffer.putInt(16).putShort((short) 1).putShort((short) 1).putInt(8_000);
        buffer.putInt(16_000).putShort((short) 2).putShort((short) 16);
        buffer.put("data".getBytes(StandardCharsets.US_ASCII)).putInt(samples * 2);
        for (int index = 0; index < samples; index++) {
            buffer.putShort((short) (Math.sin(2 * Math.PI * 440 * index / 8_000) * 8_000));
        }
        return buffer.array();
    }

    private static Sha256Digest digest(byte[] bytes) throws Exception {
        return new Sha256Digest("sha256:" + HexFormat.of().formatHex(
                MessageDigest.getInstance("SHA-256").digest(bytes)));
    }

    private static Path artifactRoot() {
        return Path.of(System.getenv().getOrDefault("TASKCAGE_ARTIFACT_ROOT", "/taskcage-work/artifacts"));
    }

    private static boolean verifiedCapsuleOutput(CapsuleExecutionResult result) {
        PublishedArtifact output = result.profileTask().artifacts().get("audio");
        if (output == null) return false;
        Path file = artifactRoot().resolve(output.path().value());
        try {
            boolean valid = Files.size(file) > 44;
            Files.deleteIfExists(file);
            Files.deleteIfExists(file.getParent());
            return valid;
        } catch (IOException exception) {
            return false;
        }
    }

    private static Scenario taskScenario(Scenario requested, int batch, int index, int concurrency) {
        if (requested == Scenario.NORMAL) return Scenario.NORMAL;
        if (requested == Scenario.TIMEOUT_CHILD) {
            return concurrency == 1 || index > 0 ? Scenario.TIMEOUT_CHILD : Scenario.NORMAL;
        }
        if (requested == Scenario.MEMORY_LIMIT) {
            return concurrency == 1 || index > 0 ? Scenario.MEMORY_LIMIT : Scenario.NORMAL;
        }

        int position = Math.floorMod(batch * concurrency + index, 10);
        return switch (position) {
            case 0 -> Scenario.TIMEOUT_CHILD;
            case 1 -> Scenario.MEMORY_LIMIT;
            default -> Scenario.NORMAL;
        };
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

    private static boolean verifiedOutput(Command command) {
        try {
            return command.outputPath != null && Files.size(command.outputPath) > 44;
        } catch (IOException exception) {
            return false;
        }
    }

    private static long elapsedMillis(long startedNanos) {
        return TimeUnit.NANOSECONDS.toMillis(System.nanoTime() - startedNanos);
    }

    private static String render(Mode mode, Scenario scenario, int concurrency, int warmupBatches,
                                 List<BatchMetric> batches, List<String> validationErrors) {
        List<TaskMetric> tasks = batches.stream().flatMap(batch -> batch.tasks.stream()).toList();
        List<TaskMetric> normalTasks = tasks.stream().filter(TaskMetric::normalWorkload).toList();
        List<Long> batchLatencies = batches.stream().map(BatchMetric::latencyMillis).sorted().toList();
        List<Long> latencies = tasks.stream().map(TaskMetric::latencyMillis).sorted().toList();
        List<Long> normalLatencies = normalTasks.stream().map(TaskMetric::latencyMillis).sorted().toList();
        Map<String, Integer> reasons = new LinkedHashMap<>();
        for (TaskMetric task : tasks) reasons.merge(task.termination, 1, Integer::sum);
        long totalMillis = batchLatencies.stream().mapToLong(Long::longValue).sum();
        long taskPeak = tasks.stream().map(TaskMetric::taskMemoryPeakBytes).filter(value -> value != null)
                .mapToLong(Long::longValue).max().orElse(-1);
        List<Long> taskCpuSamples = tasks.stream().map(TaskMetric::taskCpuTimeMicros)
                .filter(value -> value != null).toList();
        long taskCpu = taskCpuSamples.isEmpty() ? -1 : taskCpuSamples.stream().mapToLong(Long::longValue).sum();
        int residual = tasks.stream().mapToInt(TaskMetric::residualProcesses).sum();
        return "{"
                + "\"mode\":\"" + mode.jsonName + "\","
                + "\"scenario\":\"" + scenario.jsonName + "\","
                + "\"concurrency\":" + concurrency + ","
                + "\"batches\":{\"warmup\":" + warmupBatches + ",\"measured\":" + batches.size()
                + ",\"latencyMs\":{\"total\":" + totalMillis + ",\"p50\":" + percentile(batchLatencies, 0.50)
                + ",\"p95\":" + percentile(batchLatencies, 0.95) + ",\"max\":" + max(batchLatencies) + "}},"
                + "\"tasks\":{\"submitted\":" + tasks.size() + ",\"latencyMs\":{\"p50\":"
                + percentile(latencies, 0.50) + ",\"p95\":" + percentile(latencies, 0.95) + ",\"max\":"
                + max(latencies) + "},\"normalTasks\":{\"submitted\":" + normalTasks.size()
                + ",\"latencyMs\":{\"p50\":" + percentile(normalLatencies, 0.50) + ",\"p95\":"
                + percentile(normalLatencies, 0.95) + ",\"max\":" + max(normalLatencies)
                + "},\"outputsVerified\":" + normalTasks.stream().allMatch(TaskMetric::outputVerified)
                + "},\"terminationReasons\":" + reasonsJson(reasons) + "},"
                + "\"taskResources\":{\"memoryPeakBytes\":" + taskPeak + ",\"cpuTimeMicros\":" + taskCpu + "},"
                + "\"executorContainer\":{\"memoryPeakBytes\":" + cgroupMemoryPeak()
                + ",\"cpuUsageMicros\":" + cgroupCpuUsageMicros() + "},"
                + "\"cleanup\":{\"residualProcesses\":" + residual
                + ",\"cleanupConfirmed\":" + tasks.stream().allMatch(TaskMetric::cleanupConfirmed) + "},"
                + "\"validation\":{\"passed\":" + validationErrors.isEmpty()
                + ",\"errors\":" + stringsJson(validationErrors) + "}"
                + "}";
    }

    static List<String> validate(Mode mode, Scenario requestedScenario, List<TaskMetric> tasks) {
        List<String> errors = new ArrayList<>();
        if (tasks.isEmpty()) {
            errors.add("no measured tasks were produced");
            return errors;
        }
        if (requestedScenario == Scenario.TIMEOUT_CHILD
                && tasks.stream().noneMatch(task -> task.workload.equals(Scenario.TIMEOUT_CHILD.jsonName))) {
            errors.add("timeout_child scenario did not execute a timeout workload");
        }
        if (requestedScenario == Scenario.MEMORY_LIMIT
                && tasks.stream().noneMatch(task -> task.workload.equals(Scenario.MEMORY_LIMIT.jsonName))) {
            errors.add("memory_limit scenario did not execute a memory workload");
        }
        for (int index = 0; index < tasks.size(); index++) {
            TaskMetric task = tasks.get(index);
            Scenario workload;
            try {
                workload = Scenario.parse(task.workload);
            } catch (IllegalArgumentException exception) {
                errors.add("task " + index + " has unknown workload " + task.workload);
                continue;
            }
            if (workload == Scenario.MIXED_FAILURE) {
                errors.add("task " + index + " used mixed_failure as a workload instead of a request pattern");
                continue;
            }
            String expectedTermination = expectedTermination(mode, workload);
            if (!task.termination.equals(expectedTermination)) {
                errors.add("task " + index + " (" + task.workload + ") expected " + expectedTermination
                        + " but got " + task.termination);
            }
            if (workload == Scenario.NORMAL && !task.outputVerified) {
                errors.add("task " + index + " (normal) did not produce a verified WAV output");
            }
            if (mode == Mode.TASK_CAGE) {
                if (!task.cleanupConfirmed || task.residualProcesses != 0) {
                    errors.add("task " + index + " (" + task.workload + ") did not confirm cgroup cleanup");
                }
                if (task.taskCpuTimeMicros == null || task.taskCpuTimeMicros < 0
                        || task.taskMemoryPeakBytes == null || task.taskMemoryPeakBytes < 0) {
                    errors.add("task " + index + " (" + task.workload + ") has missing TaskCage usage metrics");
                }
            } else if (workload == Scenario.TIMEOUT_CHILD) {
                if (task.cleanupConfirmed || task.residualProcesses < 1) {
                    errors.add("task " + index + " (timeout_child) did not demonstrate a residual descendant");
                }
            } else if (!task.cleanupConfirmed || task.residualProcesses != 0) {
                errors.add("task " + index + " (" + task.workload + ") left unexpected residual processes");
            }
        }
        return errors;
    }

    private static String expectedTermination(Mode mode, Scenario workload) {
        if (workload == Scenario.NORMAL) return "EXITED";
        if (workload == Scenario.TIMEOUT_CHILD) {
            return mode == Mode.TASK_CAGE ? "TIMED_OUT" : "TIMED_OUT_ROOT_ONLY";
        }
        if (workload == Scenario.MEMORY_LIMIT) {
            return mode == Mode.TASK_CAGE ? "MEMORY_LIMIT_EXCEEDED" : "CANCELLED_BY_BENCHMARK";
        }
        throw new IllegalArgumentException("mixed_failure is a requested scenario, not a task workload");
    }

    private static long percentile(List<Long> values, double percentile) {
        if (values.isEmpty()) return 0;
        return values.get((int) Math.ceil(percentile * values.size()) - 1);
    }

    private static long max(List<Long> values) {
        return values.stream().mapToLong(Long::longValue).max().orElse(0);
    }

    private static String reasonsJson(Map<String, Integer> reasons) {
        return reasons.entrySet().stream().map(entry -> jsonString(entry.getKey()) + ":" + entry.getValue())
                .collect(java.util.stream.Collectors.joining(",", "{", "}"));
    }

    private static String stringsJson(List<String> values) {
        return values.stream().map(ExecutionWorkerBenchmark::jsonString)
                .collect(java.util.stream.Collectors.joining(",", "[", "]"));
    }

    private static String jsonString(String value) {
        return "\"" + value.replace("\\", "\\\\").replace("\"", "\\\"")
                .replace("\n", "\\n").replace("\r", "\\r").replace("\t", "\\t") + "\"";
    }

    private static long cgroupMemoryPeak() {
        try { return Long.parseLong(Files.readString(Path.of("/sys/fs/cgroup/memory.peak")).trim()); }
        catch (IOException | NumberFormatException ignored) { return -1; }
    }

    private static long cgroupCpuUsageMicros() {
        try {
            return Files.readAllLines(Path.of("/sys/fs/cgroup/cpu.stat")).stream()
                    .filter(line -> line.startsWith("usage_usec "))
                    .map(line -> line.substring("usage_usec ".length()))
                    .mapToLong(Long::parseLong)
                    .findFirst().orElse(-1);
        } catch (IOException | NumberFormatException ignored) {
            return -1;
        }
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

    private static int nonNegativeInt(String value) {
        int parsed = Integer.parseInt(value);
        if (parsed < 0) throw new IllegalArgumentException("BENCHMARK_WARMUP must not be negative");
        return parsed;
    }

    enum Mode {
        PROCESS_BUILDER("processbuilder"), TASK_CAGE("taskcage");
        private final String jsonName;
        Mode(String jsonName) { this.jsonName = jsonName; }
        static Mode parse(String value) { return switch (value) {
            case "processbuilder" -> PROCESS_BUILDER;
            case "taskcage" -> TASK_CAGE;
            default -> throw new IllegalArgumentException(
                    "BENCHMARK_MODE must be processbuilder or taskcage");
        }; }
    }

    enum Scenario {
        NORMAL("normal"), TIMEOUT_CHILD("timeout_child"), MEMORY_LIMIT("memory_limit"),
        MIXED_FAILURE("mixed_failure");
        private final String jsonName;
        Scenario(String jsonName) { this.jsonName = jsonName; }
        static Scenario parse(String value) { return switch (value) {
            case "normal" -> NORMAL;
            case "timeout_child" -> TIMEOUT_CHILD;
            case "memory_limit" -> MEMORY_LIMIT;
            case "mixed_failure" -> MIXED_FAILURE;
            default -> throw new IllegalArgumentException("unknown BENCHMARK_SCENARIO: " + value);
        }; }
    }

    private record Command(Path program, List<String> arguments, Path readyPath, Path outputPath) {
        private Command(Path program, List<String> arguments, Path readyPath) {
            this(program, arguments, readyPath, null);
        }
    }
    private record InputArtifact(Path file, LocalInputArtifact reference) {
        private void delete() throws IOException {
            Files.deleteIfExists(file);
            Files.deleteIfExists(file.getParent());
        }
    }
    private record BatchMetric(long latencyMillis, List<TaskMetric> tasks) {}
    record TaskMetric(long latencyMillis, String workload, String termination, Long taskCpuTimeMicros,
                      Long taskMemoryPeakBytes, int residualProcesses,
                      boolean cleanupConfirmed, boolean outputVerified) {
        private boolean normalWorkload() { return workload.equals(Scenario.NORMAL.jsonName); }
    }
}
