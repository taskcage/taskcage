package io.github.taskcage.sdk;

import java.nio.file.Path;
import java.nio.file.Files;
import java.time.Duration;
import java.time.Instant;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;

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
            TaskSubmission submission = client.submit(spec);
            Task task = (Task) submission;
            assertNotNull(task.taskId());
            assertNotNull(task.effectiveBudget());

            TaskSnapshot snapshot = client.getTask(task.taskId());
            assertEquals(task.taskId(), snapshot.taskId());
        }
    }

    @Test
    void returnsFinishedSnapshotWhenProgramCannotStart() {
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
            TaskSubmission submission = client.submit(spec);
            FinishedTaskSnapshot finished = assertInstanceOf(FinishedTaskSnapshot.class, submission);
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
            Task accepted = assertInstanceOf(Task.class, client.submit(spec));
            TaskCancellation cancellation = client.cancelTask(accepted.taskId());
            assertEquals(TaskState.FINISHED, cancellation.state());
            assertEquals(TerminationReason.CANCELLED, cancellation.terminationReason());
        }
    }

    @Test
    void reportsWallTimeLimitAfterCleanup() throws Exception {
        try (TaskCageClient client = client()) {
            Task accepted = assertInstanceOf(Task.class, client.submit(spec("/bin/sleep", List.of("10"),
                    Duration.ofMillis(100))));
            FinishedTaskSnapshot finished = awaitFinished(client, accepted.taskId());
            assertEquals(TerminationReason.TIMED_OUT, finished.result().terminationReason());
        }
    }

    @Test
    void cancellationRemovesGhostDescendants() throws Exception {
        Path ready = Files.createTempFile("taskcage-ghost-", ".ready");
        Files.deleteIfExists(ready);
        String fixture = System.getenv("TASKCAGE_GHOST_TREE");
        try (TaskCageClient client = client()) {
            Task accepted = assertInstanceOf(Task.class, client.submit(spec(fixture,
                    List.of("--hold-parent", ready.toString()), Duration.ofSeconds(20))));
            awaitFile(ready);
            List<Long> descendantPids = Files.readAllLines(ready).stream()
                    .map(line -> line.substring(line.indexOf('=') + 1))
                    .map(Long::parseLong)
                    .toList();
            client.cancelTask(accepted.taskId());
            assertEquals(true, descendantPids.stream().noneMatch(pid -> ProcessHandle.of(pid).isPresent()));
        } finally {
            Files.deleteIfExists(ready);
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

    private static FinishedTaskSnapshot awaitFinished(TaskCageClient client, java.util.UUID taskId) throws InterruptedException {
        Instant deadline = Instant.now().plusSeconds(5);
        while (Instant.now().isBefore(deadline)) {
            TaskSnapshot snapshot = client.getTask(taskId);
            if (snapshot instanceof FinishedTaskSnapshot finished) {
                return finished;
            }
            Thread.sleep(25);
        }
        throw new AssertionError("task did not finish before timeout");
    }

    private static void awaitFile(Path path) throws Exception {
        Instant deadline = Instant.now().plusSeconds(5);
        while (!Files.exists(path) && Instant.now().isBefore(deadline)) {
            Thread.sleep(25);
        }
        assertEquals(true, Files.exists(path));
    }
}
