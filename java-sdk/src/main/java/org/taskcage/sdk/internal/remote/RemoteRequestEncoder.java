package org.taskcage.sdk.internal.remote;

import com.fasterxml.jackson.core.JsonProcessingException;
import com.fasterxml.jackson.core.StreamReadFeature;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.json.JsonMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;
import java.util.Base64;
import java.util.UUID;
import org.taskcage.sdk.ManagedInputArtifact;
import org.taskcage.sdk.ProfileResourceOverrides;
import org.taskcage.sdk.RemoteBooleanInput;
import org.taskcage.sdk.RemoteInt64Input;
import org.taskcage.sdk.RemoteProfileInputValue;
import org.taskcage.sdk.RemoteProfileRequest;
import org.taskcage.sdk.RemoteStringInput;
import org.taskcage.sdk.ServiceCredentials;
import org.taskcage.sdk.Sha256Digest;
import org.taskcage.sdk.TaskCageProtocolException;

/** Encodes Remote Protocol v1 request envelopes and payloads. */
public final class RemoteRequestEncoder {
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

    public byte[] getCapabilities(UUID requestId) {
        return write(envelope(requestId, "getCapabilities", mapper.createObjectNode()));
    }

    public byte[] beginArtifactUpload(
            UUID requestId,
            UUID clientArtifactId,
            Sha256Digest digest,
            long sizeBytes,
            String mediaType) {
        if (sizeBytes <= 0) {
            throw new IllegalArgumentException("sizeBytes must be positive");
        }
        ObjectNode payload = mapper.createObjectNode();
        payload.put("clientArtifactId", clientArtifactId.toString());
        payload.put("digest", digest.value());
        payload.put("sizeBytes", sizeBytes);
        if (mediaType != null) {
            if (mediaType.isBlank()) {
                throw new IllegalArgumentException("mediaType must not be blank");
            }
            payload.put("mediaType", mediaType);
        }
        return write(envelope(requestId, "beginArtifactUpload", payload));
    }

    public byte[] uploadArtifactChunk(UUID requestId, UUID artifactId, long offset, byte[] bytes) {
        if (offset < 0 || bytes == null || bytes.length == 0) {
            throw new IllegalArgumentException("offset must be non-negative and bytes must not be empty");
        }
        ObjectNode payload = mapper.createObjectNode();
        payload.put("artifactId", artifactId.toString());
        payload.put("offset", offset);
        payload.put("dataBase64", Base64.getEncoder().encodeToString(bytes));
        return write(envelope(requestId, "uploadArtifactChunk", payload));
    }

    public byte[] completeArtifactUpload(UUID requestId, UUID artifactId) {
        return write(envelope(requestId, "completeArtifactUpload", artifactIdPayload(artifactId)));
    }

    /** Encodes the owner-only request to discard an unreferenced managed input Artifact. */
    public byte[] abortArtifactUpload(UUID requestId, UUID artifactId) {
        return write(envelope(requestId, "abortArtifactUpload", artifactIdPayload(artifactId)));
    }

    public byte[] readArtifactChunk(UUID requestId, UUID artifactId, long offset, int maxBytes) {
        if (offset < 0 || maxBytes <= 0) {
            throw new IllegalArgumentException("offset must be non-negative and maxBytes must be positive");
        }
        ObjectNode payload = artifactIdPayload(artifactId);
        payload.put("offset", offset);
        payload.put("maxBytes", maxBytes);
        return write(envelope(requestId, "readArtifactChunk", payload));
    }

    /**
     * Encodes a Profile submission with a caller-owned idempotency key.
     *
     * <p>After a lost response, callers must reuse {@code clientRequestId} with the same request. The daemon
     * resolves that key before input Artifact lookup, so this remains the only valid recovery path after a managed
     * input Artifact has transferred to task ownership.
     */
    public byte[] submitProfile(UUID requestId, UUID clientRequestId, RemoteProfileRequest request) {
        ObjectNode payload = mapper.createObjectNode();
        payload.put("clientRequestId", clientRequestId.toString());
        ObjectNode profile = payload.putObject("profile");
        profile.put("name", request.profile().name());
        profile.put("version", request.profile().version());
        ObjectNode inputs = payload.putObject("inputs");
        request.inputs().forEach((slot, value) -> encodeInput(inputs.putObject(slot), value));
        if (!request.resourceOverrides().isEmpty()) {
            encodeResourceOverrides(payload.putObject("resourceOverrides"), request.resourceOverrides());
        }
        return write(envelope(requestId, "submitProfile", payload));
    }

    public byte[] getProfileResult(UUID requestId, UUID taskId) {
        return write(envelope(requestId, "getProfileResult", taskIdPayload(taskId)));
    }

    public byte[] cancelTask(UUID requestId, UUID taskId) {
        return write(envelope(requestId, "cancelTask", taskIdPayload(taskId)));
    }

    private ObjectNode artifactIdPayload(UUID artifactId) {
        ObjectNode payload = mapper.createObjectNode();
        payload.put("artifactId", artifactId.toString());
        return payload;
    }

    private ObjectNode taskIdPayload(UUID taskId) {
        ObjectNode payload = mapper.createObjectNode();
        payload.put("taskId", taskId.toString());
        return payload;
    }

    private static void encodeInput(ObjectNode target, RemoteProfileInputValue value) {
        if (value instanceof RemoteStringInput string) {
            target.put("kind", "STRING");
            target.put("value", string.value());
        } else if (value instanceof RemoteInt64Input integer) {
            target.put("kind", "INT64");
            target.put("value", integer.value());
        } else if (value instanceof RemoteBooleanInput bool) {
            target.put("kind", "BOOLEAN");
            target.put("value", bool.value());
        } else if (value instanceof ManagedInputArtifact artifact) {
            target.put("kind", "MANAGED_INPUT");
            target.put("artifactId", artifact.artifactId().toString());
        } else {
            throw new TaskCageProtocolException("unsupported Remote Profile input value type");
        }
    }

    private static void encodeResourceOverrides(ObjectNode target, ProfileResourceOverrides overrides) {
        if (overrides.cpuMax().isPresent()
                || overrides.memoryMaxBytes().isPresent()
                || overrides.pidsMax().isPresent()
                || overrides.wallTimeLimit().isPresent()) {
            ObjectNode limits = target.putObject("limits");
            overrides.cpuMax().ifPresent(cpu -> {
                ObjectNode cpuMax = limits.putObject("cpuMax");
                cpuMax.put("quotaMicros", cpu.quotaMicros());
                cpuMax.put("periodMicros", cpu.periodMicros());
            });
            overrides.memoryMaxBytes().ifPresent(value -> limits.put("memoryMaxBytes", value));
            overrides.pidsMax().ifPresent(value -> limits.put("pidsMax", value));
            overrides.wallTimeLimit().ifPresent(value -> limits.put("wallTimeLimitMs", value.toMillis()));
        }
        if (overrides.stdoutTailMaxBytes().isPresent() || overrides.stderrTailMaxBytes().isPresent()) {
            ObjectNode output = target.putObject("output");
            overrides.stdoutTailMaxBytes().ifPresent(value -> output.put("stdoutTailMaxBytes", value));
            overrides.stderrTailMaxBytes().ifPresent(value -> output.put("stderrTailMaxBytes", value));
        }
    }

    private ObjectNode envelope(UUID requestId, String type, ObjectNode payload) {
        ObjectNode request = mapper.createObjectNode();
        request.put("remoteProtocolVersion", RemoteProtocolCodec.VERSION);
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
