package org.taskcage.sdk.internal.remote;

import com.fasterxml.jackson.core.StreamReadFeature;
import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.json.JsonMapper;
import java.io.IOException;
import java.util.UUID;
import org.taskcage.sdk.TaskCageProtocolException;

/** Parses and validates Remote Protocol v1 response envelopes. */
public final class EnvelopeValidator {
    private final ObjectMapper mapper = JsonMapper.builder()
            .enable(StreamReadFeature.STRICT_DUPLICATE_DETECTION)
            .build();

    public JsonNode readAndValidate(byte[] bytes, UUID requestId) {
        try {
            JsonNode response = mapper.readTree(bytes);
            if (!response.isObject()
                    || response.path("remoteProtocolVersion").asInt(-1) != RemoteProtocolCodec.VERSION
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
}
