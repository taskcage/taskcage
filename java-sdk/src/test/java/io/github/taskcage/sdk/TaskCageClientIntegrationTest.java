package io.github.taskcage.sdk;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.node.ObjectNode;
import com.fasterxml.jackson.databind.node.JsonNodeFactory;
import io.github.taskcage.sdk.support.FakeTaskCageServer;
import java.time.Duration;
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
            JsonNode request = server.requests().getFirst();
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
}
