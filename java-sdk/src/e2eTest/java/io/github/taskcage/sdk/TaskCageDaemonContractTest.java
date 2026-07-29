package io.github.taskcage.sdk;

import java.nio.file.Path;
import java.time.Duration;
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
            Task task = client.submit(spec);
            assertNotNull(task.taskId());
            assertNotNull(task.effectiveBudget());

            TaskSnapshot snapshot = client.getTask(task.taskId());
            assertEquals(task.taskId(), snapshot.taskId());
        }
    }
}
