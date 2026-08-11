package org.taskcage.sdk;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;
import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Duration;
import java.util.List;
import java.util.Map;
import java.util.UUID;
import java.util.function.Function;
import org.junit.jupiter.api.Test;
import org.taskcage.sdk.support.FakeTaskCageServer;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertThrows;

class ProtocolFixtureCompatibilityTest {
    private static final String FIXTURES_PROPERTY = "taskcage.protocolFixturesDir";
    private static final ObjectMapper MAPPER = new ObjectMapper();
    private static final FixtureCorpus PROTOCOL_V1 = new FixtureCorpus("v1");
    private static final UUID TASK_ID = UUID.fromString("33333333-3333-3333-3333-333333333333");

    @Test
    void protocolV1TaskAcceptedFixtureDecodes() throws Exception {
        ObjectNode fixture = PROTOCOL_V1.read("task-accepted.json");
        try (FakeTaskCageServer server = FakeTaskCageServer.start(respondWith(fixture));
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            Task task = assertInstanceOf(Task.class, client.submit(testTaskSpec()));

            assertEquals(TASK_ID, task.taskId());
            assertEquals(new CpuQuota(100_000, 100_000), task.effectiveBudget().cpuMax());
            assertEquals(536_870_912, task.effectiveBudget().memoryMaxBytes());
            assertEquals(32, task.effectiveBudget().pidsMax());
            assertEquals(Duration.ofMinutes(2), task.effectiveBudget().wallTimeLimit());
        }
    }

    @Test
    void protocolV1RunningTaskFixtureDecodes() throws Exception {
        ObjectNode fixture = PROTOCOL_V1.read("task-running.json");
        try (FakeTaskCageServer server = FakeTaskCageServer.start(respondWith(fixture));
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            RunningTaskSnapshot running = assertInstanceOf(RunningTaskSnapshot.class, client.getTask(TASK_ID));

            assertEquals(TASK_ID, running.taskId());
            assertEquals(TaskState.RUNNING, running.state());
            assertEquals("2026-07-20T09:00:00Z", running.submittedAt().toString());
            assertEquals("2026-07-20T09:00:00Z", running.startedAt().toString());
        }
    }

    @Test
    void protocolV1FinishedTaskFixturesDecode() throws Exception {
        assertFinishedFixture(
                "task-result-execution-failed.json", TerminationReason.EXECUTION_FAILED, null, null, false);
        assertFinishedFixture(
                "task-result-output-truncated.json", TerminationReason.EXITED, 0, null, true);
        assertFinishedFixture(
                "task-result-timeout.json", TerminationReason.TIMED_OUT, null, "SIGKILL", false);
    }

    @Test
    void protocolV1ErrorFixturesDecode() throws Exception {
        assertErrorFixture("error-capacity-exhausted.json", "CAPACITY_EXHAUSTED", true);
        assertErrorFixture("error-limit-exceeds-policy.json", "LIMIT_EXCEEDS_POLICY", false);
    }

    @Test
    void protocolV1DecoderIgnoresUnknownResponseFields() throws Exception {
        ObjectNode fixture = PROTOCOL_V1.read("task-result-timeout.json");
        fixture.putObject("futureEnvelope").put("revision", 2);
        ObjectNode payload = (ObjectNode) fixture.path("payload");
        payload.putObject("futurePayload").put("scheduler", "local");
        ((ObjectNode) payload.path("process")).put("futureProcessField", true);
        ((ObjectNode) payload.path("timing")).put("futureTimingField", 1);
        ((ObjectNode) payload.path("usage")).put("futureUsageField", 1);
        ((ObjectNode) payload.path("output")).put("futureOutputField", true);

        try (FakeTaskCageServer server = FakeTaskCageServer.start(respondWith(fixture));
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            FinishedTaskSnapshot finished = assertInstanceOf(FinishedTaskSnapshot.class, client.getTask(TASK_ID));

            assertEquals(TerminationReason.TIMED_OUT, finished.result().terminationReason());
            assertEquals("SIGKILL", finished.result().process().signal());
        }
    }

    private static void assertFinishedFixture(
            String fixtureName,
            TerminationReason reason,
            Integer exitCode,
            String signal,
            boolean stdoutTruncated) throws Exception {
        ObjectNode fixture = PROTOCOL_V1.read(fixtureName);
        try (FakeTaskCageServer server = FakeTaskCageServer.start(respondWith(fixture));
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            FinishedTaskSnapshot finished = assertInstanceOf(FinishedTaskSnapshot.class, client.getTask(TASK_ID));

            assertEquals(TASK_ID, finished.taskId());
            assertEquals(reason, finished.result().terminationReason());
            assertEquals(exitCode, finished.result().process().exitCode());
            assertEquals(signal, finished.result().process().signal());
            assertEquals(stdoutTruncated, finished.result().output().stdoutTruncated());
            assertFalse(finished.result().output().stderrTruncated());
        }
    }

    private static void assertErrorFixture(String fixtureName, String code, boolean retryable) throws Exception {
        ObjectNode fixture = PROTOCOL_V1.read(fixtureName);
        try (FakeTaskCageServer server = FakeTaskCageServer.start(respondWith(fixture));
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            TaskCageDaemonException error = assertThrows(
                    TaskCageDaemonException.class,
                    () -> client.submit(testTaskSpec()));

            assertEquals(code, error.code());
            assertEquals(retryable, error.retryable());
            assertFalse(error.getMessage().isBlank());
        }
    }

    private static Function<JsonNode, JsonNode> respondWith(ObjectNode fixture) {
        return request -> {
            ObjectNode response = fixture.deepCopy();
            response.put("requestId", request.path("requestId").asText());
            return response;
        };
    }

    private static TaskCageClientConfig configFor(FakeTaskCageServer server) {
        return TaskCageClientConfig.builder()
                .socketPath(server.socketPath())
                .connectTimeout(Duration.ofSeconds(1))
                .requestTimeout(Duration.ofSeconds(1))
                .build();
    }

    private static TaskSpec testTaskSpec() {
        Path base = Path.of("").toAbsolutePath();
        return new TaskSpec(
                new ExternalCommand(base.resolve("true"), List.of(), base.resolve("tmp"), Map.of()),
                new ResourceBudget(
                        new CpuQuota(100_000, 100_000),
                        536_870_912,
                        32,
                        Duration.ofMinutes(2),
                        1_024,
                        2_048));
    }

    private record FixtureCorpus(String protocolVersion) {
        ObjectNode read(String fixtureName) throws IOException {
            String configuredRoot = System.getProperty(FIXTURES_PROPERTY);
            if (configuredRoot == null || configuredRoot.isBlank()) {
                throw new IllegalStateException(FIXTURES_PROPERTY + " must point to the shared protocol fixtures");
            }

            Path versionRoot = Path.of(configuredRoot)
                    .toAbsolutePath()
                    .normalize()
                    .resolve(protocolVersion)
                    .normalize();
            Path fixturePath = versionRoot.resolve(fixtureName).normalize();
            if (!fixturePath.startsWith(versionRoot) || !Files.isRegularFile(fixturePath)) {
                throw new IOException("protocol fixture does not exist: " + fixturePath);
            }

            JsonNode fixture = MAPPER.readTree(fixturePath.toFile());
            if (!(fixture instanceof ObjectNode object)) {
                throw new IOException("protocol fixture must be a JSON object: " + fixturePath);
            }
            return object;
        }
    }
}
