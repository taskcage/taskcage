package io.github.taskcage.sdk.internal.client;

import com.fasterxml.jackson.core.JsonProcessingException;
import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;
import io.github.taskcage.sdk.TaskCageCapabilities;
import io.github.taskcage.sdk.TaskCageClient;
import io.github.taskcage.sdk.TaskCageClientConfig;
import io.github.taskcage.sdk.TaskCageConnectionException;
import io.github.taskcage.sdk.TaskCageProtocolException;
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
    private final ObjectMapper mapper = new ObjectMapper();
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
            request.set("payload", mapper.createObjectNode());

            byte[] responseBytes = requireConnection().request(writeRequest(request), config.requestTimeout());
            JsonNode response = mapper.readTree(responseBytes);
            validateResponse(response, requestId);
            return response;
        } catch (IOException exception) {
            closeConnection();
            throw new TaskCageConnectionException("TaskCage daemon connection failed", exception);
        } finally {
            requestLock.unlock();
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
