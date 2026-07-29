package io.github.taskcage.sdk.internal.client;

import com.fasterxml.jackson.core.JsonProcessingException;
import com.fasterxml.jackson.core.StreamReadFeature;
import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.json.JsonMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;
import io.github.taskcage.sdk.TaskCageCapabilities;
import io.github.taskcage.sdk.TaskCageClient;
import io.github.taskcage.sdk.TaskCageClientConfig;
import io.github.taskcage.sdk.TaskCageConnectionException;
import io.github.taskcage.sdk.TaskCageProtocolException;
import io.github.taskcage.sdk.Task;
import io.github.taskcage.sdk.TaskSpec;
import io.github.taskcage.sdk.ResourceBudget;
import io.github.taskcage.sdk.internal.transport.UnixDomainSocketConnection;
import java.io.IOException;
import java.util.ArrayList;
import java.util.List;
import java.util.UUID;
import java.util.concurrent.locks.ReentrantLock;

/** Serializes Protocol v1 requests over one lazily connected Unix domain socket. */
public final class DefaultTaskCageClient implements TaskCageClient {
    private static final int PROTOCOL_VERSION = 1;

    private final TaskCageClientConfig config;
    private final ObjectMapper mapper = JsonMapper.builder()
            .enable(StreamReadFeature.STRICT_DUPLICATE_DETECTION)
            .build();
    private final ReentrantLock requestLock = new ReentrantLock();
    private UnixDomainSocketConnection connection;
    private boolean closed;

    public DefaultTaskCageClient(TaskCageClientConfig config) {
        this.config = config;
    }

    @Override
    public TaskCageCapabilities capabilities() {
        TaskCageConnectionException lastFailure = null;
        for (int attempt = 0; attempt < 2; attempt++) {
            try {
                return decodeCapabilities(request("getCapabilities"));
            } catch (TaskCageConnectionException exception) {
                lastFailure = exception;
            }
        }
        throw lastFailure;
    }

    @Override
    public Task submit(TaskSpec task) {
        ObjectNode payload = mapper.createObjectNode();
        payload.put("clientRequestId", UUID.randomUUID().toString());
        ObjectNode command = payload.putObject("command");
        command.put("program", task.command().program().toString());
        command.putPOJO("args", task.command().arguments());
        command.put("workingDirectory", task.command().workingDirectory().toString());
        command.set("environment", mapper.valueToTree(task.command().environment()));
        ResourceBudget budget = task.budget();
        ObjectNode limits = payload.putObject("limits");
        ObjectNode cpuMax = limits.putObject("cpuMax");
        cpuMax.put("quotaMicros", budget.cpuMax().quotaMicros());
        cpuMax.put("periodMicros", budget.cpuMax().periodMicros());
        limits.put("memoryMaxBytes", budget.memoryMaxBytes());
        limits.put("pidsMax", budget.pidsMax());
        limits.put("wallTimeLimitMs", budget.wallTimeLimit().toMillis());
        ObjectNode output = payload.putObject("output");
        output.put("stdoutTailMaxBytes", budget.stdoutTailMaxBytes());
        output.put("stderrTailMaxBytes", budget.stderrTailMaxBytes());
        return decodeAccepted(request("submitTask", payload), budget);
    }

    @Override
    public void close() {
        requestLock.lock();
        try {
            if (closed) {
                return;
            }
            closed = true;
            closeConnection();
        } finally {
            requestLock.unlock();
        }
    }

    private JsonNode request(String type) {
        return request(type, mapper.createObjectNode());
    }

    private JsonNode request(String type, ObjectNode payload) {
        requestLock.lock();
        try {
            if (closed) {
                throw new IllegalStateException("TaskCageClient is closed");
            }
            String requestId = UUID.randomUUID().toString();
            ObjectNode request = mapper.createObjectNode();
            request.put("protocolVersion", PROTOCOL_VERSION);
            request.put("requestId", requestId);
            request.put("type", type);
            request.set("payload", payload);

            byte[] responseBytes = requireConnection().request(writeRequest(request), config.requestTimeout());
            JsonNode response = mapper.readTree(responseBytes);
            validateResponse(response, requestId);
            return response;
        } catch (JsonProcessingException exception) {
            closeConnection();
            throw new TaskCageProtocolException("invalid JSON response from TaskCage daemon", exception);
        } catch (IOException exception) {
            closeConnection();
            throw new TaskCageConnectionException("TaskCage daemon connection failed", exception);
        } finally {
            requestLock.unlock();
        }
    }

    private Task decodeAccepted(JsonNode response, ResourceBudget requestedBudget) {
        if (!"taskAccepted".equals(response.path("type").asText())
                || !"RUNNING".equals(response.path("payload").path("state").asText())) {
            throw new TaskCageProtocolException("expected taskAccepted response");
        }
        JsonNode limits = response.path("payload").path("effectiveLimits");
        try {
            return new Task(UUID.fromString(requiredText(response.path("payload"), "taskId")), new ResourceBudget(
                    new io.github.taskcage.sdk.CpuQuota(requiredPositiveLong(limits.path("cpuMax"), "quotaMicros"), requiredPositiveLong(limits.path("cpuMax"), "periodMicros")),
                    requiredPositiveLong(limits, "memoryMaxBytes"), requiredPositiveLong(limits, "pidsMax"),
                    java.time.Duration.ofMillis(requiredPositiveLong(limits, "wallTimeLimitMs")),
                    requestedBudget.stdoutTailMaxBytes(), requestedBudget.stderrTailMaxBytes()));
        } catch (IllegalArgumentException exception) {
            throw new TaskCageProtocolException("invalid taskAccepted payload", exception);
        }
    }

    private UnixDomainSocketConnection requireConnection() throws IOException {
        if (connection == null) {
            connection = UnixDomainSocketConnection.connect(config.socketPath(), config.connectTimeout());
        }
        return connection;
    }

    private byte[] writeRequest(ObjectNode request) {
        try {
            return mapper.writeValueAsBytes(request);
        } catch (JsonProcessingException exception) {
            throw new TaskCageProtocolException("could not encode Protocol v1 request", exception);
        }
    }

    private void validateResponse(JsonNode response, String requestId) {
        if (!response.isObject()
                || response.path("protocolVersion").asInt(-1) != PROTOCOL_VERSION
                || !requestId.equals(response.path("requestId").asText())
                || !response.has("type")
                || !response.path("payload").isObject()) {
            throw new TaskCageProtocolException("invalid Protocol v1 response envelope");
        }
    }

    private TaskCageCapabilities decodeCapabilities(JsonNode response) {
        if (!"capabilities".equals(response.path("type").asText())) {
            throw new TaskCageProtocolException("expected capabilities response");
        }
        JsonNode payload = response.path("payload");
        List<Integer> versions = new ArrayList<>();
        if (!payload.path("protocolVersions").isArray()) {
            throw new TaskCageProtocolException("capabilities.protocolVersions must be an array");
        }
        for (JsonNode version : payload.path("protocolVersions")) {
            if (!version.canConvertToInt()) {
                throw new TaskCageProtocolException("capabilities.protocolVersions must contain integers");
            }
            versions.add(version.intValue());
        }
        try {
            return new TaskCageCapabilities(
                    requiredText(payload, "daemonVersion"),
                    versions,
                    requiredPositiveInt(payload, "maxFrameBytes"),
                    requiredPositiveInt(payload, "maxConcurrentTasks"),
                    requiredBoolean(payload, "cgroupV2Ready"));
        } catch (IllegalArgumentException exception) {
            throw new TaskCageProtocolException("invalid capabilities payload", exception);
        }
    }

    private static String requiredText(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || !value.isTextual() || value.textValue().isEmpty()) {
            throw new IllegalArgumentException(field + " must be a non-empty string");
        }
        return value.textValue();
    }

    private static int requiredPositiveInt(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || !value.canConvertToInt() || value.intValue() <= 0) {
            throw new IllegalArgumentException(field + " must be a positive integer");
        }
        return value.intValue();
    }

    private static long requiredPositiveLong(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || !value.canConvertToLong() || value.longValue() <= 0) {
            throw new IllegalArgumentException(field + " must be a positive integer");
        }
        return value.longValue();
    }

    private static boolean requiredBoolean(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || !value.isBoolean()) {
            throw new IllegalArgumentException(field + " must be a boolean");
        }
        return value.booleanValue();
    }

    private void closeConnection() {
        if (connection == null) {
            return;
        }
        try {
            connection.close();
        } catch (IOException ignored) {
            // The channel is being discarded after a failure or explicit close.
        } finally {
            connection = null;
        }
    }
}
