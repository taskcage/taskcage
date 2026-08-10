package io.github.taskcage.sdk;

import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Duration;
import java.time.Instant;
import java.util.List;
import java.util.Map;
import java.util.UUID;
import java.util.concurrent.TimeUnit;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.Timeout;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertTrue;

class FfmpegReferenceWorkflowTest {
    private static final Duration RESULT_TIMEOUT = Duration.ofSeconds(15);
    private static final Duration POLL_INTERVAL = Duration.ofMillis(25);

    @Test
    @Timeout(20)
    void directFfmpegRawCommandProducesWaveArtifact() throws Exception {
        Path output = Files.createTempFile(workDirectory(), "taskcage-ffmpeg-", ".wav");
        Files.deleteIfExists(output);
        TaskSpec spec = new TaskSpec(
                new ExternalCommand(
                        ffmpeg(),
                        List.of(
                                "-hide_banner",
                                "-loglevel",
                                "error",
                                "-nostdin",
                                "-y",
                                "-f",
                                "lavfi",
                                "-i",
                                "sine=frequency=1000:duration=0.25",
                                "-c:a",
                                "pcm_s16le",
                                output.toString()),
                        workDirectory(),
                        Map.of("LANG", "C.UTF-8")),
                referenceBudget(Duration.ofSeconds(30)));

        try (TaskCageClient client = client()) {
            FinishedTaskSnapshot finished = awaitFinished(
                    client, client.submit(UUID.randomUUID(), spec), RESULT_TIMEOUT);

            assertEquals(TerminationReason.EXITED, finished.result().terminationReason());
            assertEquals(0, finished.result().process().exitCode());
            byte[] header = Files.readAllBytes(output);
            assertTrue(header.length > 44, "FFmpeg must produce a non-empty WAVE file");
            assertEquals("RIFF", new String(header, 0, 4, StandardCharsets.US_ASCII));
            assertEquals("WAVE", new String(header, 8, 4, StandardCharsets.US_ASCII));
        } finally {
            Files.deleteIfExists(output);
        }
    }

    @Test
    @Timeout(20)
    void taskCageTimeoutRemovesTheRealFfmpegDescendant() throws Exception {
        Path ready = Files.createTempFile(workDirectory(), "taskcage-ffmpeg-tree-", ".ready");
        Files.deleteIfExists(ready);
        TaskSpec spec = new TaskSpec(
                new ExternalCommand(
                        ffmpegTree(),
                        List.of("--ffmpeg", ffmpeg().toString(), "--ready", ready.toString()),
                        workDirectory(),
                        Map.of("LANG", "C.UTF-8")),
                referenceBudget(Duration.ofSeconds(2)));

        try (TaskCageClient client = client()) {
            Task task = assertInstanceOf(Task.class, client.submit(UUID.randomUUID(), spec));
            long ffmpegPid = awaitFfmpegPid(ready, Duration.ofSeconds(5));
            assertTrue(isAlive(ffmpegPid), "FFmpeg descendant must be alive before timeout");

            FinishedTaskSnapshot finished = awaitFinished(client, task, RESULT_TIMEOUT);

            assertEquals(TerminationReason.TIMED_OUT, finished.result().terminationReason());
            awaitGone(ffmpegPid, Duration.ofSeconds(5));
        } finally {
            Files.deleteIfExists(ready);
        }
    }

    @Test
    @Timeout(20)
    void processBuilderRootOnlyTerminationLeavesTheSameFfmpegDescendant() throws Exception {
        Path ready = Files.createTempFile(workDirectory(), "process-builder-ffmpeg-tree-", ".ready");
        Files.deleteIfExists(ready);
        Process root = new ProcessBuilder(
                        ffmpegTree().toString(),
                        "--ffmpeg",
                        ffmpeg().toString(),
                        "--ready",
                        ready.toString())
                .directory(workDirectory().toFile())
                .redirectOutput(ProcessBuilder.Redirect.DISCARD)
                .redirectError(ProcessBuilder.Redirect.DISCARD)
                .start();
        long ffmpegPid = -1;
        try {
            ffmpegPid = awaitFfmpegPid(ready, Duration.ofSeconds(5));
            assertTrue(isAlive(ffmpegPid), "FFmpeg descendant must be alive before root termination");

            root.destroyForcibly();
            assertTrue(root.waitFor(5, TimeUnit.SECONDS), "ProcessBuilder root must terminate");

            assertTrue(isAlive(ffmpegPid), "root-only termination must leave the FFmpeg descendant alive");
        } finally {
            root.destroyForcibly();
            root.waitFor(5, TimeUnit.SECONDS);
            if (ffmpegPid > 0) {
                ProcessHandle.of(ffmpegPid).ifPresent(ProcessHandle::destroyForcibly);
                awaitGone(ffmpegPid, Duration.ofSeconds(5));
            }
            Files.deleteIfExists(ready);
        }
    }

    private static ResourceBudget referenceBudget(Duration wallTime) {
        return new ResourceBudget(
                new CpuQuota(100_000, 100_000),
                512L * 1024 * 1024,
                128,
                wallTime,
                4_096,
                4_096);
    }

    private static FinishedTaskSnapshot awaitFinished(
            TaskCageClient client, TaskSubmission submission, Duration timeout) throws InterruptedException {
        if (submission instanceof FinishedTaskSnapshot finished) {
            return finished;
        }
        Task task = assertInstanceOf(Task.class, submission);
        return awaitFinished(client, task, timeout);
    }

    private static FinishedTaskSnapshot awaitFinished(
            TaskCageClient client, Task task, Duration timeout) throws InterruptedException {
        Instant deadline = Instant.now().plus(timeout);
        while (Instant.now().isBefore(deadline)) {
            TaskSnapshot snapshot = client.getTask(task.taskId());
            if (snapshot instanceof FinishedTaskSnapshot finished) {
                return finished;
            }
            Thread.sleep(POLL_INTERVAL.toMillis());
        }
        throw new AssertionError("Task did not finish before the reference-workflow deadline");
    }

    private static long awaitFfmpegPid(Path ready, Duration timeout) throws Exception {
        Instant deadline = Instant.now().plus(timeout);
        while (!Files.exists(ready) && Instant.now().isBefore(deadline)) {
            Thread.sleep(POLL_INTERVAL.toMillis());
        }
        assertTrue(Files.exists(ready), "FFmpeg launcher did not publish its descendant PID");
        String value = Files.readString(ready, StandardCharsets.UTF_8).trim();
        return Long.parseLong(value.substring(value.indexOf('=') + 1));
    }

    private static void awaitGone(long pid, Duration timeout) throws InterruptedException {
        Instant deadline = Instant.now().plus(timeout);
        while (isAlive(pid) && Instant.now().isBefore(deadline)) {
            Thread.sleep(POLL_INTERVAL.toMillis());
        }
        assertTrue(!isAlive(pid), "process must not remain after explicit cleanup: " + pid);
    }

    private static boolean isAlive(long pid) {
        return ProcessHandle.of(pid).map(ProcessHandle::isAlive).orElse(false);
    }

    private static TaskCageClient client() {
        return TaskCageClient.connect(TaskCageClientConfig.builder()
                .socketPath(Path.of(requiredEnvironment("TASKCAGE_SOCKET")))
                .build());
    }

    private static Path ffmpeg() {
        return Path.of(requiredEnvironment("TASKCAGE_FFMPEG"));
    }

    private static Path ffmpegTree() {
        return Path.of(requiredEnvironment("TASKCAGE_FFMPEG_TREE"));
    }

    private static Path workDirectory() {
        return Path.of(requiredEnvironment("TASKCAGE_FFMPEG_WORK_DIR"));
    }

    private static String requiredEnvironment(String name) {
        String value = System.getenv(name);
        if (value == null || value.isBlank()) {
            throw new IllegalStateException(name + " is required");
        }
        return value;
    }
}
