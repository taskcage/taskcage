package io.github.taskcage.sdk;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.node.ObjectNode;
import com.fasterxml.jackson.databind.node.JsonNodeFactory;
import io.github.taskcage.sdk.support.FakeTaskCageServer;
import java.time.Duration;
import java.nio.charset.StandardCharsets;
import java.util.List;
import java.util.function.Function;
import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

class TaskCageClientIntegrationTest {
    @Test
    void capabilitiesUsesProtocolV1OverUnixDomainSocket() throws Exception {
        try (FakeTaskCageServer server = FakeTaskCageServer.start(TaskCageClientIntegrationTest::capabilitiesResponse);
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            TaskCageCapabilities capabilities = client.capabilities();

            assertEquals("0.1.0", capabilities.daemonVersion());
            assertEquals(List.of(1), capabilities.protocolVersions());
            assertEquals(4, capabilities.maxConcurrentTasks());
            assertTrue(capabilities.cgroupV2Ready());

            server.awaitRequests(Duration.ofSeconds(2));
            JsonNode request = server.requests().get(0);
            assertEquals(1, request.path("protocolVersion").asInt());
            assertEquals("getCapabilities", request.path("type").asText());
            assertTrue(request.path("payload").isObject());
        }
    }

    @Test
    void capabilitiesReconnectsOnceAfterResponseIsLost() throws Exception {
        Function<JsonNode, JsonNode> closeWithoutResponse = ignored -> null;
        try (FakeTaskCageServer server = FakeTaskCageServer.start(
                List.of(closeWithoutResponse, TaskCageClientIntegrationTest::capabilitiesResponse));
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            TaskCageCapabilities capabilities = client.capabilities();

            assertTrue(capabilities.cgroupV2Ready());
            server.awaitRequests(Duration.ofSeconds(2));
            assertEquals(2, server.requests().size());
        }
    }

    @Test
    void mismatchedRequestIdIsReportedAsProtocolFailure() throws Exception {
        try (FakeTaskCageServer server = FakeTaskCageServer.start(request -> {
            ObjectNode response = capabilitiesResponse(request);
            response.put("requestId", "different-request-id");
            return response;
        });
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            assertThrows(TaskCageProtocolException.class, client::capabilities);
            server.awaitRequests(Duration.ofSeconds(2));
        }
    }

    @Test
    void malformedJsonResponseIsReportedAsProtocolFailureWithoutRetrying() throws Exception {
        try (FakeTaskCageServer server = FakeTaskCageServer.startRaw(ignored -> "{".getBytes(StandardCharsets.UTF_8));
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            assertThrows(TaskCageProtocolException.class, client::capabilities);
            server.awaitRequests(Duration.ofSeconds(2));
            assertEquals(1, server.requests().size());
        }
    }

    @Test
    void duplicateJsonKeysAreReportedAsProtocolFailure() throws Exception {
        try (FakeTaskCageServer server = FakeTaskCageServer.startRaw(request -> (
                "{\"protocolVersion\":1,\"requestId\":\"" + request.path("requestId").asText()
                        + "\",\"requestId\":\"other\",\"type\":\"capabilities\",\"payload\":{}}")
                .getBytes(StandardCharsets.UTF_8));
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            assertThrows(TaskCageProtocolException.class, client::capabilities);
            server.awaitRequests(Duration.ofSeconds(2));
        }
    }

    @Test
    void submitEncodesMandatoryLimitsAndDecodesAcceptedTask() throws Exception {
        try (FakeTaskCageServer server = FakeTaskCageServer.start(TaskCageClientIntegrationTest::taskAcceptedResponse);
                TaskCageClient client = TaskCageClient.connect(configFor(server))) {
            Task task = client.submit(new TaskSpec(
                    new ExternalCommand(java.nio.file.Path.of("/usr/bin/true"), List.of(),
                            java.nio.file.Path.of("/srv/taskcage/jobs/42"), java.util.Map.of("LANG", "C.UTF-8")),
                    new ResourceBudget(new CpuQuota(100_000, 100_000), 536_870_912, 32,
                            Duration.ofMinutes(2), 1_024, 2_048)));

            assertEquals("b5309d98-f51e-45e1-9866-b1a080c1ba50", task.taskId().toString());
            assertEquals(1_024, task.effectiveBudget().stdoutTailMaxBytes());
            server.awaitRequests(Duration.ofSeconds(2));
            JsonNode payload = server.requests().get(0).path("payload");
            assertEquals("submitTask", server.requests().get(0).path("type").asText());
            assertEquals("/usr/bin/true", payload.path("command").path("program").asText());
            assertEquals(536_870_912, payload.path("limits").path("memoryMaxBytes").asLong());
        }
    }

    private static TaskCageClientConfig configFor(FakeTaskCageServer server) {
        return TaskCageClientConfig.builder()
                .socketPath(server.socketPath())
                .connectTimeout(Duration.ofSeconds(1))
                .requestTimeout(Duration.ofSeconds(1))
                .build();
    }

    private static ObjectNode capabilitiesResponse(JsonNode request) {
        ObjectNode response = JsonNodeFactory.instance.objectNode();
        response.put("protocolVersion", 1);
        response.put("requestId", request.path("requestId").asText());
        response.put("type", "capabilities");
        ObjectNode payload = response.putObject("payload");
        payload.put("daemonVersion", "0.1.0");
        payload.putArray("protocolVersions").add(1);
        payload.put("maxFrameBytes", 1_048_576);
        payload.put("maxConcurrentTasks", 4);
        payload.put("cgroupV2Ready", request.path("requestId").asText().length() > 0);
        return response;
    }

    private static ObjectNode taskAcceptedResponse(JsonNode request) {
        ObjectNode response = JsonNodeFactory.instance.objectNode();
        response.put("protocolVersion", 1);
        response.put("requestId", request.path("requestId").asText());
        response.put("type", "taskAccepted");
        ObjectNode payload = response.putObject("payload");
        payload.put("taskId", "b5309d98-f51e-45e1-9866-b1a080c1ba50");
        payload.put("state", "RUNNING");
        ObjectNode limits = payload.putObject("effectiveLimits");
        ObjectNode cpu = limits.putObject("cpuMax");
        cpu.put("quotaMicros", 100_000);
        cpu.put("periodMicros", 100_000);
        limits.put("memoryMaxBytes", 536_870_912);
        limits.put("pidsMax", 32);
        limits.put("wallTimeLimitMs", 120_000);
        return response;
    }
}
