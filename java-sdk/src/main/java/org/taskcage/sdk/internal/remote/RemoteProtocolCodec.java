package org.taskcage.sdk.internal.remote;

import com.fasterxml.jackson.core.JsonProcessingException;
import com.fasterxml.jackson.core.StreamReadFeature;
import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.json.JsonMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;
import java.io.IOException;
import java.time.Instant;
import java.util.Base64;
import java.util.List;
import java.util.Map;
import java.util.UUID;
import org.taskcage.sdk.CpuQuota;
import org.taskcage.sdk.ManagedOutputArtifact;
import org.taskcage.sdk.ProfileIdentity;
import org.taskcage.sdk.ProfileResourceOverrides;
import org.taskcage.sdk.RemoteArtifactUpload;
import org.taskcage.sdk.RemoteArtifactUploadStart;
import org.taskcage.sdk.RemoteArtifactUploadState;
import org.taskcage.sdk.RemoteArtifactChunkProgress;
import org.taskcage.sdk.RemoteBooleanInput;
import org.taskcage.sdk.RemoteCapabilities;
import org.taskcage.sdk.RemoteInt64Input;
import org.taskcage.sdk.RemoteProfileInputValue;
import org.taskcage.sdk.RemoteProfileRequest;
import org.taskcage.sdk.RemoteStringInput;
import org.taskcage.sdk.ManagedInputArtifact;
import org.taskcage.sdk.ExecutionResult;
import org.taskcage.sdk.FinishedRemoteProfileTaskSnapshot;
import org.taskcage.sdk.ProcessResult;
import org.taskcage.sdk.ProfileFailure;
import org.taskcage.sdk.ProfileOutcome;
import org.taskcage.sdk.RemoteProfileTask;
import org.taskcage.sdk.RemoteProfileTaskSnapshot;
import org.taskcage.sdk.ResourceBudget;
import org.taskcage.sdk.RunningRemoteProfileTaskSnapshot;
import org.taskcage.sdk.Sha256Digest;
import org.taskcage.sdk.TaskOutput;
import org.taskcage.sdk.TaskTiming;
import org.taskcage.sdk.TaskUsage;
import org.taskcage.sdk.TerminationReason;
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

    public RemoteCapabilities decodeCapabilities(JsonNode response) {
        requireType(response, "capabilities");
        JsonNode payload = response.path("payload");
        return new RemoteCapabilities(
                requiredText(payload, "daemonVersion"),
                requiredIntegerList(payload, "remoteProtocolVersions"),
                requiredPositiveInt(payload, "maxFrameBytes"),
                requiredTextList(payload, "artifactModes"),
                requiredPositiveLong(payload, "maxArtifactBytes"),
                requiredPositiveInt(payload, "maxArtifactChunkBytes"),
                requiredPositiveLong(payload, "artifactRetentionSeconds"));
    }

    public RemoteArtifactUpload decodeArtifactUploaded(JsonNode response) {
        requireType(response, "artifactUploaded");
        JsonNode payload = response.path("payload");
        try {
            return new RemoteArtifactUpload(
                    UUID.fromString(requiredText(payload, "artifactId")),
                    new Sha256Digest(requiredText(payload, "digest")),
                    requiredPositiveLong(payload, "sizeBytes"),
                    Instant.parse(requiredText(payload, "expiresAt")));
        } catch (IllegalArgumentException exception) {
            throw new TaskCageProtocolException("invalid artifactUploaded response payload", exception);
        }
    }

    /** Decodes a begin-upload response that is also used to recover after a lost acknowledgement. */
    public RemoteArtifactUploadStart decodeArtifactUploadStarted(JsonNode response) {
        requireType(response, "artifactUploadStarted");
        JsonNode payload = response.path("payload");
        try {
            return new RemoteArtifactUploadStart(
                    UUID.fromString(requiredText(payload, "artifactId")),
                    enumValue(payload, "state", RemoteArtifactUploadState.class),
                    requiredNonNegativeLong(payload, "nextOffset"));
        } catch (IllegalArgumentException exception) {
            throw new TaskCageProtocolException("invalid artifactUploadStarted response payload", exception);
        }
    }

    /** Decodes a chunk acknowledgement, including an idempotent acknowledgement after retry. */
    public RemoteArtifactChunkProgress decodeArtifactChunkAccepted(JsonNode response) {
        requireType(response, "artifactChunkAccepted");
        JsonNode payload = response.path("payload");
        try {
            return new RemoteArtifactChunkProgress(
                    UUID.fromString(requiredText(payload, "artifactId")),
                    requiredNonNegativeLong(payload, "nextOffset"));
        } catch (IllegalArgumentException exception) {
            throw new TaskCageProtocolException("invalid artifactChunkAccepted response payload", exception);
        }
    }

    public ManagedOutputArtifact decodeManagedOutputArtifact(JsonNode artifact) {
        if (!artifact.isObject() || !"MANAGED_OUTPUT".equals(artifact.path("kind").asText())) {
            throw new TaskCageProtocolException("expected MANAGED_OUTPUT Artifact");
        }
        try {
            return new ManagedOutputArtifact(
                    UUID.fromString(requiredText(artifact, "artifactId")),
                    new Sha256Digest(requiredText(artifact, "digest")),
                    requiredNonNegativeLong(artifact, "sizeBytes"),
                    requiredText(artifact, "mediaType"),
                    Instant.parse(requiredText(artifact, "expiresAt")));
        } catch (IllegalArgumentException exception) {
            throw new TaskCageProtocolException("invalid MANAGED_OUTPUT Artifact", exception);
        }
    }

    public RemoteProfileTask decodeProfileAccepted(JsonNode response) {
        requireType(response, "profileAccepted");
        JsonNode payload = response.path("payload");
        if (!"RUNNING".equals(payload.path("state").asText())) {
            throw new TaskCageProtocolException("expected RUNNING profileAccepted response");
        }
        try {
            return new RemoteProfileTask(
                    UUID.fromString(requiredText(payload, "taskId")),
                    decodeProfile(payload.path("profile")),
                    decodeEffectiveResources(payload.path("effectiveResources")));
        } catch (IllegalArgumentException exception) {
            throw new TaskCageProtocolException("invalid profileAccepted response payload", exception);
        }
    }

    public RemoteProfileTaskSnapshot decodeProfileResult(JsonNode response) {
        requireType(response, "profileResult");
        JsonNode payload = response.path("payload");
        try {
            UUID taskId = UUID.fromString(requiredText(payload, "taskId"));
            ProfileIdentity profile = decodeProfile(payload.path("profile"));
            return switch (requiredText(payload, "state")) {
                case "RUNNING" -> new RunningRemoteProfileTaskSnapshot(
                        taskId, profile, Instant.parse(requiredText(payload, "submittedAt")),
                        Instant.parse(requiredText(payload, "startedAt")));
                case "FINISHED" -> decodeFinishedProfileResult(taskId, profile, payload);
                default -> throw new IllegalArgumentException("state must be RUNNING or FINISHED");
            };
        } catch (IllegalArgumentException exception) {
            throw new TaskCageProtocolException("invalid remote profileResult payload", exception);
        }
    }

    private FinishedRemoteProfileTaskSnapshot decodeFinishedProfileResult(
            UUID taskId, ProfileIdentity profile, JsonNode payload) {
        ProfileOutcome outcome = enumValue(payload, "profileOutcome", ProfileOutcome.class);
        Map<String, ManagedOutputArtifact> artifacts = new java.util.TreeMap<>();
        JsonNode artifactNodes = requiredObject(payload, "artifacts");
        artifactNodes.properties().forEach(entry -> artifacts.put(entry.getKey(), decodeManagedOutputArtifact(entry.getValue())));
        ProfileFailure failure = null;
        if (outcome == ProfileOutcome.FAILED) {
            JsonNode failureNode = requiredObject(payload, "failure");
            failure = new ProfileFailure(requiredText(failureNode, "code"), requiredText(failureNode, "message"));
        } else if (payload.has("failure")) {
            throw new IllegalArgumentException("successful result must not contain failure");
        }
        return new FinishedRemoteProfileTaskSnapshot(taskId, profile, outcome, decodeExecutionResult(payload), artifacts, failure);
    }

    private static ProfileIdentity decodeProfile(JsonNode value) {
        return new ProfileIdentity(requiredText(value, "name"), requiredText(value, "version"));
    }

    private static ResourceBudget decodeEffectiveResources(JsonNode value) {
        JsonNode limits = requiredObject(value, "limits");
        JsonNode output = requiredObject(value, "output");
        JsonNode cpuMax = requiredObject(limits, "cpuMax");
        return new ResourceBudget(
                new CpuQuota(requiredPositiveLong(cpuMax, "quotaMicros"), requiredPositiveLong(cpuMax, "periodMicros")),
                requiredPositiveLong(limits, "memoryMaxBytes"),
                requiredPositiveLong(limits, "pidsMax"),
                java.time.Duration.ofMillis(requiredPositiveLong(limits, "wallTimeLimitMs")),
                requiredPositiveInt(output, "stdoutTailMaxBytes"),
                requiredPositiveInt(output, "stderrTailMaxBytes"));
    }

    private static ExecutionResult decodeExecutionResult(JsonNode payload) {
        JsonNode process = requiredObject(payload, "process");
        JsonNode timing = requiredObject(payload, "timing");
        JsonNode usage = requiredObject(payload, "usage");
        JsonNode output = requiredObject(payload, "output");
        return new ExecutionResult(
                enumValue(payload, "terminationReason", TerminationReason.class),
                new ProcessResult(optionalInt(process, "exitCode"), optionalText(process, "signal")),
                new TaskTiming(
                        Instant.parse(requiredText(timing, "submittedAt")),
                        Instant.parse(requiredText(timing, "startedAt")),
                        Instant.parse(requiredText(timing, "finishedAt")),
                        java.time.Duration.ofMillis(requiredNonNegativeLong(timing, "wallTimeMs"))),
                new TaskUsage(requiredNonNegativeLong(usage, "cpuTimeMicros"), requiredNonNegativeLong(usage, "memoryPeakBytes")),
                new TaskOutput(requiredString(output, "stdoutTail"), requiredString(output, "stderrTail"),
                        requiredBoolean(output, "stdoutTruncated"), requiredBoolean(output, "stderrTruncated")));
    }

    private ObjectNode artifactIdPayload(UUID artifactId) {
        ObjectNode payload = mapper.createObjectNode();
        payload.put("artifactId", artifactId.toString());
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

    private static void requireType(JsonNode response, String type) {
        if (!type.equals(response.path("type").asText())) {
            throw new TaskCageProtocolException("expected " + type + " response");
        }
    }

    private static String requiredText(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || !value.isTextual() || value.textValue().isEmpty()) {
            throw new IllegalArgumentException(field + " must be a non-empty string");
        }
        return value.textValue();
    }

    private static String requiredString(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || !value.isTextual()) {
            throw new IllegalArgumentException(field + " must be a string");
        }
        return value.textValue();
    }

    private static JsonNode requiredObject(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || !value.isObject()) {
            throw new IllegalArgumentException(field + " must be an object");
        }
        return value;
    }

    private static boolean requiredBoolean(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || !value.isBoolean()) {
            throw new IllegalArgumentException(field + " must be a boolean");
        }
        return value.booleanValue();
    }

    private static Integer optionalInt(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || value.isNull()) {
            return null;
        }
        if (!value.isIntegralNumber() || !value.canConvertToInt()) {
            throw new IllegalArgumentException(field + " must be an integer or null");
        }
        return value.intValue();
    }

    private static String optionalText(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || value.isNull()) {
            return null;
        }
        if (!value.isTextual() || value.textValue().isEmpty()) {
            throw new IllegalArgumentException(field + " must be a non-empty string or null");
        }
        return value.textValue();
    }

    private static <T extends Enum<T>> T enumValue(JsonNode object, String field, Class<T> type) {
        try {
            return Enum.valueOf(type, requiredText(object, field));
        } catch (IllegalArgumentException exception) {
            throw new IllegalArgumentException(field + " is invalid", exception);
        }
    }

    private static int requiredPositiveInt(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || !value.isIntegralNumber() || !value.canConvertToInt() || value.intValue() <= 0) {
            throw new IllegalArgumentException(field + " must be a positive integer");
        }
        return value.intValue();
    }

    private static long requiredPositiveLong(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || !value.isIntegralNumber() || !value.canConvertToLong() || value.longValue() <= 0) {
            throw new IllegalArgumentException(field + " must be a positive integer");
        }
        return value.longValue();
    }

    private static long requiredNonNegativeLong(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || !value.isIntegralNumber() || !value.canConvertToLong() || value.longValue() < 0) {
            throw new IllegalArgumentException(field + " must be a non-negative integer");
        }
        return value.longValue();
    }

    private static List<Integer> requiredIntegerList(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || !value.isArray()) {
            throw new IllegalArgumentException(field + " must be an array");
        }
        java.util.ArrayList<Integer> values = new java.util.ArrayList<>();
        for (JsonNode entry : value) {
            if (!entry.isIntegralNumber() || !entry.canConvertToInt()) {
                throw new IllegalArgumentException(field + " must contain integers");
            }
            values.add(entry.intValue());
        }
        return values;
    }

    private static List<String> requiredTextList(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || !value.isArray()) {
            throw new IllegalArgumentException(field + " must be an array");
        }
        java.util.ArrayList<String> values = new java.util.ArrayList<>();
        for (JsonNode entry : value) {
            if (!entry.isTextual() || entry.textValue().isEmpty()) {
                throw new IllegalArgumentException(field + " must contain non-empty strings");
            }
            values.add(entry.textValue());
        }
        return values;
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
