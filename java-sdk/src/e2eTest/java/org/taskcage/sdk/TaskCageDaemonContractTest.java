package org.taskcage.sdk;

import java.nio.file.Path;
import java.nio.file.Files;
import java.time.Duration;
import java.time.Instant;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertEquals;

class TaskCageDaemonContractTest {
    @Test
    void submitsTaskToRunningDaemon() {
        String socketPath = System.getenv("TASKCAGE_SOCKET");
        TaskSpec spec = new TaskSpec(
                new ExternalCommand(
                        Path.of("/usr/bin/true"),
                        List.of(),
                        Path.of("/tmp"),
                        Map.of("LANG", "C.UTF-8")),
                new ResourceBudget(
                        new CpuQuota(100_000, 100_000),
                        64L * 1024 * 1024,
                        8,
                        Duration.ofSeconds(10),
                        1_024,
                        1_024));

        try (TaskCageClient client = TaskCageClient.connect(
                TaskCageClientConfig.builder().socketPath(Path.of(socketPath)).build())) {
            TaskHandle handle = client.submitHandle(spec);
            assertNotNull(handle.taskId());

            TaskSnapshot snapshot = handle.get();
            assertEquals(handle.taskId(), snapshot.taskId());
        }
    }

    @Test
    void returnsFinishedSnapshotWhenProgramCannotStart() throws Exception {
        String socketPath = System.getenv("TASKCAGE_SOCKET");
        TaskSpec spec = new TaskSpec(
                new ExternalCommand(
                        Path.of("/usr/bin/taskcage-program-does-not-exist"),
                        List.of(),
                        Path.of("/tmp"),
                        Map.of("LANG", "C.UTF-8")),
                new ResourceBudget(
                        new CpuQuota(100_000, 100_000),
                        64L * 1024 * 1024,
                        8,
                        Duration.ofSeconds(10),
                        1_024,
                        1_024));

        try (TaskCageClient client = TaskCageClient.connect(
                TaskCageClientConfig.builder().socketPath(Path.of(socketPath)).build())) {
            FinishedTaskSnapshot finished = client.run(spec, Duration.ofSeconds(5));
            assertEquals(TerminationReason.EXECUTION_FAILED, finished.result().terminationReason());
        }
    }

    @Test
    void cancelsRunningTaskAfterCleanup() {
        String socketPath = System.getenv("TASKCAGE_SOCKET");
        TaskSpec spec = new TaskSpec(
                new ExternalCommand(
                        Path.of("/bin/sleep"),
                        List.of("10"),
                        Path.of("/tmp"),
                        Map.of("LANG", "C.UTF-8")),
                new ResourceBudget(
                        new CpuQuota(100_000, 100_000),
                        64L * 1024 * 1024,
                        8,
                        Duration.ofSeconds(20),
                        1_024,
                        1_024));

        try (TaskCageClient client = TaskCageClient.connect(
                TaskCageClientConfig.builder().socketPath(Path.of(socketPath)).build())) {
            TaskCancellation cancellation = client.submitHandle(spec).cancel();
            assertEquals(TaskState.FINISHED, cancellation.state());
            assertEquals(TerminationReason.CANCELLED, cancellation.terminationReason());
        }
    }

    @Test
    void reportsWallTimeLimitAfterCleanup() throws Exception {
        try (TaskCageClient client = client()) {
            FinishedTaskSnapshot finished = client.run(
                    spec("/bin/sleep", List.of("10"), Duration.ofMillis(100)), Duration.ofSeconds(5));
            assertEquals(TerminationReason.TIMED_OUT, finished.result().terminationReason());
        }
    }

    @Test
    void cancellationRemovesGhostDescendants() throws Exception {
        Path ready = Files.createTempFile("taskcage-ghost-", ".ready");
        Files.deleteIfExists(ready);
        String fixture = System.getenv("TASKCAGE_GHOST_TREE");
        try (TaskCageClient client = client()) {
            TaskHandle handle = client.submitHandle(spec(
                    fixture, List.of("--hold-parent", ready.toString()), Duration.ofSeconds(20)));
            awaitFile(ready);
            List<Long> descendantPids = Files.readAllLines(ready).stream()
                    .map(line -> line.substring(line.indexOf('=') + 1))
                    .map(Long::parseLong)
                    .toList();
            handle.cancel();
            assertEquals(true, descendantPids.stream().noneMatch(pid -> ProcessHandle.of(pid).isPresent()));
        } finally {
            Files.deleteIfExists(ready);
        }
    }

    @Test
    void retainsBoundedOutputTailsAndTruncationFlags() throws Exception {
        String fixture = System.getenv("TASKCAGE_OUTPUT_FLOOD");
        TaskSpec spec = new TaskSpec(
                new ExternalCommand(Path.of(fixture), List.of("both"), Path.of("/tmp"), Map.of("LANG", "C.UTF-8")),
                new ResourceBudget(new CpuQuota(100_000, 100_000), 64L * 1024 * 1024, 8,
                        Duration.ofSeconds(10), 64, 64));
        try (TaskCageClient client = client()) {
            FinishedTaskSnapshot finished = client.run(spec, Duration.ofSeconds(5));
            assertEquals(TerminationReason.EXITED, finished.result().terminationReason());
            assertEquals(true, finished.result().output().stdoutTruncated());
            assertEquals(true, finished.result().output().stderrTruncated());
            assertEquals(true, finished.result().output().stdoutTail().contains("STDOUT-END"));
            assertEquals(true, finished.result().output().stderrTail().contains("STDERR-END"));
        }
    }

    @Test
    void reusesTaskForSameClientRequestId() {
        java.util.UUID requestId = java.util.UUID.randomUUID();
        try (TaskCageClient client = client()) {
            TaskSpec spec = spec("/bin/sleep", List.of("1"), Duration.ofSeconds(5));
            TaskHandle first = client.submitHandle(requestId, spec);
            TaskHandle second = client.submitHandle(requestId, spec);
            assertEquals(first.taskId(), second.taskId());
        }
    }

    private static TaskCageClient client() {
        return TaskCageClient.connect(TaskCageClientConfig.builder()
                .socketPath(Path.of(System.getenv("TASKCAGE_SOCKET"))).build());
    }

    private static TaskSpec spec(String program, List<String> args, Duration wallTime) {
        return new TaskSpec(new ExternalCommand(Path.of(program), args, Path.of("/tmp"), Map.of("LANG", "C.UTF-8")),
                new ResourceBudget(new CpuQuota(100_000, 100_000), 64L * 1024 * 1024, 8, wallTime, 1_024, 1_024));
    }

    private static void awaitFile(Path path) throws Exception {
        Instant deadline = Instant.now().plusSeconds(5);
        while (!Files.exists(path) && Instant.now().isBefore(deadline)) {
            Thread.sleep(25);
        }
        assertEquals(true, Files.exists(path));
    }
}
