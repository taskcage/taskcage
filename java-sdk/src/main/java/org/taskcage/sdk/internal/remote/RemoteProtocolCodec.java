package org.taskcage.sdk.internal.remote;

import com.fasterxml.jackson.core.JsonProcessingException;
import com.fasterxml.jackson.core.StreamReadFeature;
import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.json.JsonMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;
import java.io.IOException;
import java.util.UUID;
import org.taskcage.sdk.ServiceCredentials;
import org.taskcage.sdk.TaskCageDaemonException;
import org.taskcage.sdk.TaskCageProtocolException;

/** Encoder and envelope validator for Remote Protocol v1. */
public final class RemoteProtocolCodec {
    public static final int VERSION = 1;

    private final ObjectMapper mapper = JsonMapper.builder()
            .enable(StreamReadFeature.STRICT_DUPLICATE_DETECTION)
            .build();

    public byte[] authenticate(UUID requestId, ServiceCredentials credentials) {
        ObjectNode payload = mapper.createObjectNode();
        payload.put("clientId", credentials.clientId());
        char[] secret = credentials.secret().copyCharacters();
        try {
            payload.put("secret", new String(secret));
        } finally {
            java.util.Arrays.fill(secret, '\0');
        }
        return write(envelope(requestId, "authenticate", payload));
    }

    public JsonNode readAndValidate(byte[] bytes, UUID requestId) {
        try {
            JsonNode response = mapper.readTree(bytes);
            if (!response.isObject()
                    || response.path("remoteProtocolVersion").asInt(-1) != VERSION
                    || !requestId.toString().equals(response.path("requestId").asText())
                    || !response.path("type").isTextual()
                    || !response.path("payload").isObject()) {
                throw new TaskCageProtocolException("invalid Remote Protocol v1 response envelope");
            }
            return response;
        } catch (IOException exception) {
            throw new TaskCageProtocolException("invalid JSON response from TaskCage remote daemon", exception);
        }
    }

    public void requireAuthenticated(JsonNode response) {
        if (!"authenticated".equals(response.path("type").asText())) {
            throw new TaskCageProtocolException("expected authenticated response");
        }
        JsonNode payload = response.path("payload");
        if (!payload.path("principal").isTextual()
                || payload.path("principal").asText().isBlank()
                || !payload.path("sessionExpiresAt").isTextual()
                || payload.path("sessionExpiresAt").asText().isBlank()) {
            throw new TaskCageProtocolException("invalid authenticated response payload");
        }
    }

    public TaskCageDaemonException decodeError(JsonNode response) {
        if (!"error".equals(response.path("type").asText())) {
            throw new TaskCageProtocolException("expected remote error response");
        }
        JsonNode payload = response.path("payload");
        if (!payload.path("code").isTextual()
                || payload.path("code").asText().isBlank()
                || !payload.path("message").isTextual()
                || !payload.path("retryable").isBoolean()) {
            throw new TaskCageProtocolException("invalid remote error payload");
        }
        return new TaskCageDaemonException(
                payload.path("code").asText(),
                payload.path("message").asText(),
                payload.path("retryable").asBoolean());
    }

    private ObjectNode envelope(UUID requestId, String type, ObjectNode payload) {
        ObjectNode request = mapper.createObjectNode();
        request.put("remoteProtocolVersion", VERSION);
        request.put("requestId", requestId.toString());
        request.put("type", type);
        request.set("payload", payload);
        return request;
    }

    private byte[] write(ObjectNode request) {
        try {
            return mapper.writeValueAsBytes(request);
        } catch (JsonProcessingException exception) {
            throw new TaskCageProtocolException("could not encode Remote Protocol v1 request", exception);
        }
    }
}
