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
    private static final String DOCKER = "docker";
    private static final String DOCKER_TASK_METRICS = "TASKCAGE_DOCKER_TASK_METRICS";
    private static final CapsuleIdentity FFMPEG_CAPSULE =
            new CapsuleIdentity("ffmpeg-audio-to-wav", "1.0.0");
    private static final CapsuleIdentity FFMPEG_VIDEO_CAPSULE =
            new CapsuleIdentity("ffmpeg-video-transcode", "1.0.0");
    private static final CapsuleIdentity GHOST_TREE_CAPSULE =
            new CapsuleIdentity("ghost-tree-timeout", "1.0.0");
    private static final CapsuleIdentity MEMORY_HOG_CAPSULE =
            new CapsuleIdentity("memory-hog-limit", "1.0.0");

    private ExecutionWorkerBenchmark() {}

    public static void main(String[] args) throws Exception {
        Mode mode = Mode.parse(requiredEnv("BENCHMARK_MODE"));
        Scenario scenario = Scenario.parse(requiredEnv("BENCHMARK_SCENARIO"));
        NormalWorkload normalWorkload = NormalWorkload.parse(
                System.getenv().getOrDefault("BENCHMARK_NORMAL_WORKLOAD", "audio_to_wav"));
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
                List<TaskMetric> tasks = run(
                        mode, scenario, batch, concurrency, workDirectory, source, normalWorkload);
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
                                        InputArtifact source, NormalWorkload normalWorkload)
            throws InterruptedException, ExecutionException {
        ExecutorService pool = Executors.newFixedThreadPool(concurrency);
        try {
            List<Future<TaskMetric>> futures = new ArrayList<>();
            for (int index = 0; index < concurrency; index++) {
                int taskIndex = index;
                Callable<TaskMetric> task = switch (mode) {
                    case PROCESS_BUILDER -> () -> runProcessBuilder(
                            scenario, batch, taskIndex, concurrency, workDirectory, source, normalWorkload);
                    case DOCKER_PER_TASK -> () -> runDockerPerTask(
                            scenario, batch, taskIndex, concurrency, workDirectory, source, normalWorkload);
                    case TASK_CAGE -> () -> runTaskCage(
                            scenario, batch, taskIndex, concurrency, workDirectory, source, normalWorkload);
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
                                                Path workDirectory, InputArtifact source,
                                                NormalWorkload normalWorkload) throws Exception {
        Scenario taskScenario = taskScenario(scenario, batch, index, concurrency);
        Command command = commandFor(taskScenario, index, workDirectory, source.file(), normalWorkload);
        long startedNanos = System.nanoTime();
        List<String> processArguments = new ArrayList<>();
        processArguments.add(command.program.toString());
        processArguments.addAll(command.arguments);
        Process process = new ProcessBuilder(processArguments)
                .directory(workDirectory.toFile())
                .redirectOutput(ProcessBuilder.Redirect.DISCARD)
                .redirectError(ProcessBuilder.Redirect.DISCARD)
                .start();
        ProcessUsageSampler usage = taskScenario == Scenario.NORMAL
                ? ProcessUsageSampler.start(process.pid()) : null;

        List<ProcessHandle> descendants = List.of();
        String termination;
        try {
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
                if (process.waitFor(normalWorkload.waitTimeout().toMillis(), TimeUnit.MILLISECONDS)) {
                    termination = process.exitValue() == 0 ? "EXITED" : "EXITED_NON_ZERO";
                } else {
                    process.destroyForcibly();
                    process.waitFor(5, TimeUnit.SECONDS);
                    termination = "TIMED_OUT_BY_BENCHMARK";
                }
            }

            int residual = alive(descendants);
            descendants.forEach(handle -> {
                if (handle.isAlive()) handle.destroyForcibly();
            });
            Long cpuMicros = taskScenario == Scenario.NORMAL ? usage.cpuTimeMicros() : null;
            return new TaskMetric(elapsedMillis(startedNanos), taskScenario.jsonName, termination, cpuMicros,
                    usage == null ? null : usage.memoryPeakBytes(), residual, residual == 0,
                    taskScenario != Scenario.NORMAL || verifiedOutput(command));
        } finally {
            if (usage != null) usage.close();
        }
    }

    private static TaskMetric runTaskCage(Scenario scenario, int batch, int index, int concurrency,
                                          Path workDirectory, InputArtifact source,
                                          NormalWorkload normalWorkload) throws Exception {
        Scenario taskScenario = taskScenario(scenario, batch, index, concurrency);
        long startedNanos = System.nanoTime();
        try (TaskCageClient client = TaskCageClient.connect(TaskCageClientConfig.builder()
                .socketPath(Path.of(requiredEnv("TASKCAGE_SOCKET")))
                .connectTimeout(Duration.ofSeconds(2))
                .requestTimeout(Duration.ofSeconds(5))
                .build());
             CapsuleRunner runner = CapsuleRunner.external(client)) {
            CapsuleExecutionResult finished = runner.execute(
                    UUID.randomUUID(), requestFor(taskScenario, source.reference(), normalWorkload),
                    normalWorkload.waitTimeout());
            ExecutionResult result = finished.execution();
            boolean outputVerified = taskScenario != Scenario.NORMAL
                    || verifiedCapsuleOutput(finished, normalWorkload);
            return new TaskMetric(elapsedMillis(startedNanos), taskScenario.jsonName, result.terminationReason().name(),
                    result.usage().cpuTimeMicros(), result.usage().memoryPeakBytes(), 0, true,
                    outputVerified);
        }
    }

    private static TaskMetric runDockerPerTask(Scenario scenario, int batch, int index, int concurrency,
                                               Path workDirectory, InputArtifact source,
                                               NormalWorkload normalWorkload) throws Exception {
        Scenario taskScenario = taskScenario(scenario, batch, index, concurrency);
        Command taskCommand = commandFor(taskScenario, index, workDirectory, source.file(), normalWorkload);
        String containerName = "taskcage-benchmark-" + UUID.randomUUID();
        long startedNanos = System.nanoTime();

        if (taskScenario == Scenario.TIMEOUT_CHILD) {
            Process process = startDocker(dockerRun(containerName, "128m", 32, GHOST_TREE.toString(),
                    taskCommand.arguments));
            try {
                awaitReady(taskCommand.readyPath, Duration.ofSeconds(3));
                runDocker(List.of(DOCKER, "stop", "--time", "0", containerName));
                if (!process.waitFor(10, TimeUnit.SECONDS)) {
                    process.destroyForcibly();
                    throw new IllegalStateException("docker timeout task did not stop");
                }
                boolean removed = dockerContainerAbsent(containerName);
                return new TaskMetric(elapsedMillis(startedNanos), taskScenario.jsonName,
                        "TIMED_OUT_CONTAINER", null, null, 0, removed, true);
            } finally {
                process.destroyForcibly();
                removeDockerContainer(containerName);
            }
        }

        if (taskScenario == Scenario.MEMORY_LIMIT) {
            String containerId = runDocker(dockerRunDetached(containerName, "16m", 8, MEMORY_HOG.toString(),
                    taskCommand.arguments)).trim();
            try {
                runDocker(List.of(DOCKER, "wait", containerId));
                boolean oomKilled = Boolean.parseBoolean(runDocker(List.of(
                        DOCKER, "inspect", "--format", "{{.State.OOMKilled}}", containerId)).trim());
                removeDockerContainer(containerName);
                return new TaskMetric(elapsedMillis(startedNanos), taskScenario.jsonName,
                        oomKilled ? "MEMORY_LIMIT_EXCEEDED" : "EXITED_NON_ZERO", null, null,
                        0, dockerContainerAbsent(containerName), true);
            } finally {
                removeDockerContainer(containerName);
            }
        }

        Process process = startDocker(dockerRun(containerName, "1024m", 32,
                "/usr/local/bin/taskcage-docker-task-exec", dockerCommand(taskCommand)));
        String output = readProcessOutput(process, normalWorkload.waitTimeout());
        DockerTaskMetrics metrics = parseDockerTaskMetrics(output);
        boolean removed = dockerContainerAbsent(containerName);
        return new TaskMetric(elapsedMillis(startedNanos), taskScenario.jsonName,
                metrics.exitCode == 0 ? "EXITED" : "EXITED_NON_ZERO", metrics.cpuTimeMicros,
                metrics.memoryPeakBytes, 0, removed, verifiedOutput(taskCommand));
    }

    private static List<String> dockerRun(String name, String memory, int pids, String entrypoint,
                                          List<String> arguments) {
        List<String> command = dockerRunBase(name, memory, pids);
        command.add("--rm");
        command.add("--entrypoint");
        command.add(entrypoint);
        command.add(requiredEnv("BENCHMARK_TASK_IMAGE"));
        command.addAll(arguments);
        return command;
    }

    private static List<String> dockerRunDetached(String name, String memory, int pids, String entrypoint,
                                                  List<String> arguments) {
        List<String> command = dockerRunBase(name, memory, pids);
        command.add("--detach");
        command.add("--entrypoint");
        command.add(entrypoint);
        command.add(requiredEnv("BENCHMARK_TASK_IMAGE"));
        command.addAll(arguments);
        return command;
    }

    private static List<String> dockerRunBase(String name, String memory, int pids) {
        return new ArrayList<>(List.of(DOCKER, "run", "--name", name, "--cpus", "1.0",
                "--memory", memory, "--pids-limit", Integer.toString(pids),
                "--workdir", "/taskcage-work", "--volume",
                requiredEnv("BENCHMARK_WORK_VOLUME") + ":/taskcage-work"));
    }

    private static List<String> dockerCommand(Command command) {
        List<String> arguments = new ArrayList<>();
        arguments.add(command.program.toString());
        arguments.addAll(command.arguments);
        return arguments;
    }

    private static Process startDocker(List<String> arguments) throws IOException {
        return new ProcessBuilder(arguments).redirectErrorStream(true).start();
    }

    private static String runDocker(List<String> arguments) throws Exception {
        Process process = startDocker(arguments);
        return readProcessOutput(process, Duration.ofSeconds(15));
    }

    private static String readProcessOutput(Process process, Duration timeout) throws Exception {
        if (!process.waitFor(timeout.toMillis(), TimeUnit.MILLISECONDS)) {
            process.destroyForcibly();
            throw new IllegalStateException("docker command did not complete within " + timeout);
        }
        String output = new String(process.getInputStream().readAllBytes(), StandardCharsets.UTF_8);
        if (process.exitValue() != 0) {
            throw new IllegalStateException("docker command failed (" + process.exitValue() + "): " + output);
        }
        return output;
    }

    private static DockerTaskMetrics parseDockerTaskMetrics(String output) {
        for (String line : output.lines().toList()) {
            String[] values = line.trim().split("\\s+");
            if (values.length == 4 && values[0].equals(DOCKER_TASK_METRICS)) {
                try {
                    return new DockerTaskMetrics(Long.parseLong(values[1]), Long.parseLong(values[2]),
                            Integer.parseInt(values[3]));
                } catch (NumberFormatException exception) {
                    throw new IllegalStateException("invalid Docker task metrics: " + line, exception);
                }
            }
        }
        throw new IllegalStateException("Docker task metrics were not emitted: " + output);
    }

    private static boolean dockerContainerAbsent(String name) throws Exception {
        Process process = startDocker(List.of(DOCKER, "container", "inspect", name));
        if (!process.waitFor(10, TimeUnit.SECONDS)) {
            process.destroyForcibly();
            return false;
        }
        return process.exitValue() != 0;
    }

    private static void removeDockerContainer(String name) {
        try {
            Process process = startDocker(List.of(DOCKER, "rm", "--force", name));
            process.waitFor(10, TimeUnit.SECONDS);
        } catch (IOException ignored) {
            // Best effort only: the benchmark reports a failed cleanup assertion if it survives.
        } catch (InterruptedException ignored) {
            Thread.currentThread().interrupt();
        }
    }

    private static CapsuleRequest requestFor(
            Scenario scenario, LocalInputArtifact source, NormalWorkload normalWorkload) {
        return switch (scenario) {
            case NORMAL -> normalWorkload.request(source);
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

    private static Command commandFor(
            Scenario scenario, int index, Path workDirectory, Path source, NormalWorkload normalWorkload) {
        if (scenario == Scenario.TIMEOUT_CHILD) {
            Path ready = workDirectory.resolve("ghost-" + index + ".ready");
            return new Command(GHOST_TREE, List.of("--hold-parent", ready.toString()), ready);
        }
        if (scenario == Scenario.MEMORY_LIMIT) {
            return new Command(MEMORY_HOG, List.of("67108864", "30"), null);
        }
        return normalWorkload.command(source, workDirectory.resolve("normal-" + index + normalWorkload.extension()));
    }

    private static InputArtifact createInputArtifact() throws Exception {
        String directory = "benchmark-inputs/" + UUID.randomUUID();
        Path supplied = optionalInputFile();
        ArtifactPath path = new ArtifactPath(directory + "/source" + extension(supplied));
        Path file = artifactRoot().resolve(path.value());
        Files.createDirectories(file.getParent());
        if (supplied == null) {
            Files.write(file, wave());
        } else {
            Files.copy(supplied, file);
        }
        long size = Files.size(file);
        return new InputArtifact(file, new LocalInputArtifact(path, digest(file), size));
    }

    private static Path optionalInputFile() {
        String value = System.getenv("BENCHMARK_INPUT_FILE");
        if (value == null || value.isBlank()) return null;
        Path file = Path.of(value);
        if (!Files.isRegularFile(file)) {
            throw new IllegalArgumentException("BENCHMARK_INPUT_FILE must point to a regular file");
        }
        return file;
    }

    private static String extension(Path input) {
        if (input == null) return ".wav";
        String name = input.getFileName().toString();
        int dot = name.lastIndexOf('.');
        return dot > 0 ? name.substring(dot) : ".bin";
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

    private static Sha256Digest digest(Path file) throws Exception {
        MessageDigest digest = MessageDigest.getInstance("SHA-256");
        try (var input = Files.newInputStream(file)) {
            byte[] buffer = new byte[16 * 1024];
            for (int read; (read = input.read(buffer)) >= 0; ) {
                digest.update(buffer, 0, read);
            }
        }
        return new Sha256Digest("sha256:" + HexFormat.of().formatHex(digest.digest()));
    }

    private static Path artifactRoot() {
        return Path.of(System.getenv().getOrDefault("TASKCAGE_ARTIFACT_ROOT", "/taskcage-work/artifacts"));
    }

    private static boolean verifiedCapsuleOutput(
            CapsuleExecutionResult result, NormalWorkload normalWorkload) {
        PublishedArtifact output = result.profileTask().artifacts().get(normalWorkload.outputName());
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

    private static final class ProcessUsageSampler implements AutoCloseable {
        private final long pid;
        private final Thread thread;
        private volatile boolean running = true;
        private volatile long memoryPeakBytes = -1;
        private volatile long cpuTimeMicros = -1;

        private ProcessUsageSampler(long pid) {
            this.pid = pid;
            thread = new Thread(this::sample, "taskcage-benchmark-process-usage");
            thread.setDaemon(true);
            thread.start();
        }

        static ProcessUsageSampler start(long pid) {
            return new ProcessUsageSampler(pid);
        }

        private void sample() {
            while (running) {
                memoryPeakBytes = Math.max(memoryPeakBytes, readMemoryPeak(pid));
                cpuTimeMicros = Math.max(cpuTimeMicros, readCpuTimeMicros(pid));
                try {
                    Thread.sleep(10);
                } catch (InterruptedException exception) {
                    Thread.currentThread().interrupt();
                    return;
                }
            }
            memoryPeakBytes = Math.max(memoryPeakBytes, readMemoryPeak(pid));
            cpuTimeMicros = Math.max(cpuTimeMicros, readCpuTimeMicros(pid));
        }

        private long memoryPeakBytes() {
            return memoryPeakBytes;
        }

        private long cpuTimeMicros() {
            return cpuTimeMicros;
        }

        @Override
        public void close() {
            running = false;
            thread.interrupt();
            try {
                thread.join(100);
            } catch (InterruptedException exception) {
                Thread.currentThread().interrupt();
            }
        }

        private static long readMemoryPeak(long pid) {
            try {
                for (String line : Files.readAllLines(Path.of("/proc", Long.toString(pid), "status"))) {
                    if (line.startsWith("VmHWM:") || line.startsWith("VmRSS:")) {
                        String[] fields = line.trim().split("\\s+");
                        return Long.parseLong(fields[1]) * 1024;
                    }
                }
            } catch (IOException | NumberFormatException ignored) {
                // The process can exit between samples; retain the last observed peak.
            }
            return -1;
        }

        private static long readCpuTimeMicros(long pid) {
            try {
                String stat = Files.readString(Path.of("/proc", Long.toString(pid), "stat"));
                int commandEnd = stat.lastIndexOf(')');
                String[] fields = stat.substring(commandEnd + 1).trim().split("\\s+");
                long jiffies = Long.parseLong(fields[11]) + Long.parseLong(fields[12]);
                return TimeUnit.MILLISECONDS.toMicros(jiffies * 10);
            } catch (IOException | NumberFormatException | IndexOutOfBoundsException ignored) {
                return -1;
            }
        }
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
            } else if (mode == Mode.PROCESS_BUILDER && workload == Scenario.TIMEOUT_CHILD) {
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
            return switch (mode) {
                case TASK_CAGE -> "TIMED_OUT";
                case DOCKER_PER_TASK -> "TIMED_OUT_CONTAINER";
                case PROCESS_BUILDER -> "TIMED_OUT_ROOT_ONLY";
            };
        }
        if (workload == Scenario.MEMORY_LIMIT) {
            return mode == Mode.PROCESS_BUILDER ? "CANCELLED_BY_BENCHMARK" : "MEMORY_LIMIT_EXCEEDED";
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
        PROCESS_BUILDER("processbuilder"), DOCKER_PER_TASK("docker_per_task"), TASK_CAGE("taskcage");
        private final String jsonName;
        Mode(String jsonName) { this.jsonName = jsonName; }
        static Mode parse(String value) { return switch (value) {
            case "processbuilder" -> PROCESS_BUILDER;
            case "docker_per_task" -> DOCKER_PER_TASK;
            case "taskcage" -> TASK_CAGE;
            default -> throw new IllegalArgumentException(
                    "BENCHMARK_MODE must be processbuilder, docker_per_task or taskcage");
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

    enum NormalWorkload {
        AUDIO_TO_WAV("audio_to_wav", ".wav", "audio", FFMPEG_CAPSULE),
        VIDEO_TRANSCODE("video_transcode", ".mp4", "video", FFMPEG_VIDEO_CAPSULE);

        private final String value;
        private final String extension;
        private final String outputName;
        private final CapsuleIdentity capsule;

        NormalWorkload(String value, String extension, String outputName, CapsuleIdentity capsule) {
            this.value = value;
            this.extension = extension;
            this.outputName = outputName;
            this.capsule = capsule;
        }

        static NormalWorkload parse(String value) {
            return switch (value) {
                case "audio_to_wav" -> AUDIO_TO_WAV;
                case "video_transcode" -> VIDEO_TRANSCODE;
                default -> throw new IllegalArgumentException(
                        "BENCHMARK_NORMAL_WORKLOAD must be audio_to_wav or video_transcode");
            };
        }

        private CapsuleRequest request(LocalInputArtifact source) {
            if (this == AUDIO_TO_WAV) {
                return CapsuleRequest.builder(capsule)
                        .artifact("source", source)
                        .int64("sample_rate_hz", 16_000)
                        .int64("channels", 1)
                        .build();
            }
            return CapsuleRequest.builder(capsule).artifact("source", source).build();
        }

        private Command command(Path source, Path output) {
            if (this == AUDIO_TO_WAV) {
                return new Command(FFMPEG, List.of(
                        "-hide_banner", "-loglevel", "error", "-nostdin", "-y",
                        "-i", source.toString(), "-map", "0:a:0", "-vn", "-c:a", "pcm_s16le",
                        "-ar", "16000", "-ac", "1", output.toString()), null, output);
            }
            return new Command(FFMPEG, List.of(
                    "-hide_banner", "-loglevel", "error", "-nostdin", "-y",
                    "-i", source.toString(), "-map", "0:v:0", "-map", "0:a?",
                    "-vf", "scale=-2:480",
                    "-c:v", "libx264", "-preset", "ultrafast", "-crf", "28", "-threads", "1",
                    "-c:a", "aac", output.toString()), null, output);
        }

        private String extension() {
            return extension;
        }

        private String outputName() {
            return outputName;
        }

        private Duration waitTimeout() {
            return this == VIDEO_TRANSCODE ? Duration.ofMinutes(10) : Duration.ofSeconds(25);
        }
    }

    private record Command(Path program, List<String> arguments, Path readyPath, Path outputPath) {
        private Command(Path program, List<String> arguments, Path readyPath) {
            this(program, arguments, readyPath, null);
        }
    }
    private record DockerTaskMetrics(long memoryPeakBytes, long cpuTimeMicros, int exitCode) {}
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
