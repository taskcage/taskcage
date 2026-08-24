package org.taskcage.sdk.internal.remote;

import static org.taskcage.sdk.internal.protocol.common.JsonFields.enumValue;
import static org.taskcage.sdk.internal.protocol.common.JsonFields.nullableInt;
import static org.taskcage.sdk.internal.protocol.common.JsonFields.nullableText;
import static org.taskcage.sdk.internal.protocol.common.JsonFields.requiredBoolean;
import static org.taskcage.sdk.internal.protocol.common.JsonFields.requiredIntegerList;
import static org.taskcage.sdk.internal.protocol.common.JsonFields.requiredNonNegativeLong;
import static org.taskcage.sdk.internal.protocol.common.JsonFields.requiredNonBlankText;
import static org.taskcage.sdk.internal.protocol.common.JsonFields.requiredObject;
import static org.taskcage.sdk.internal.protocol.common.JsonFields.requiredPositiveInt;
import static org.taskcage.sdk.internal.protocol.common.JsonFields.requiredPositiveLong;
import static org.taskcage.sdk.internal.protocol.common.JsonFields.requiredString;
import static org.taskcage.sdk.internal.protocol.common.JsonFields.requiredText;
import static org.taskcage.sdk.internal.protocol.common.JsonFields.requiredTextList;

import com.fasterxml.jackson.databind.JsonNode;
import java.time.Instant;
import java.util.Base64;
import java.util.Map;
import java.util.UUID;
import org.taskcage.sdk.CpuQuota;
import org.taskcage.sdk.ExecutionResult;
import org.taskcage.sdk.FinishedRemoteProfileTaskSnapshot;
import org.taskcage.sdk.ManagedOutputArtifact;
import org.taskcage.sdk.ProcessResult;
import org.taskcage.sdk.ProfileFailure;
import org.taskcage.sdk.ProfileIdentity;
import org.taskcage.sdk.ProfileOutcome;
import org.taskcage.sdk.RemoteArtifactChunk;
import org.taskcage.sdk.RemoteArtifactChunkProgress;
import org.taskcage.sdk.RemoteArtifactUpload;
import org.taskcage.sdk.RemoteArtifactUploadStart;
import org.taskcage.sdk.RemoteArtifactUploadState;
import org.taskcage.sdk.RemoteCapabilities;
import org.taskcage.sdk.RemoteProfileTask;
import org.taskcage.sdk.RemoteProfileTaskSnapshot;
import org.taskcage.sdk.ResourceBudget;
import org.taskcage.sdk.RunningRemoteProfileTaskSnapshot;
import org.taskcage.sdk.Sha256Digest;
import org.taskcage.sdk.TaskCageDaemonException;
import org.taskcage.sdk.TaskCageProtocolException;
import org.taskcage.sdk.TaskCancellation;
import org.taskcage.sdk.TaskOutput;
import org.taskcage.sdk.TaskTiming;
import org.taskcage.sdk.TaskUsage;
import org.taskcage.sdk.TerminationReason;

/** Decodes validated Remote Protocol v1 response payloads. */
public final class RemoteResponseDecoder {
    public void requireAuthenticated(JsonNode response) {
        if (!"authenticated".equals(response.path("type").asText())) {
            throw new TaskCageProtocolException("expected authenticated response");
        }
        JsonNode payload = response.path("payload");
        try {
            requiredNonBlankText(payload, "principal");
            requiredNonBlankText(payload, "sessionExpiresAt");
        } catch (IllegalArgumentException exception) {
            throw new TaskCageProtocolException("invalid authenticated response payload");
        }
    }

    public TaskCageDaemonException decodeError(JsonNode response) {
        if (!"error".equals(response.path("type").asText())) {
            throw new TaskCageProtocolException("expected remote error response");
        }
        JsonNode payload = response.path("payload");
        try {
            return new TaskCageDaemonException(
                    requiredNonBlankText(payload, "code"),
                    requiredString(payload, "message"),
                    requiredBoolean(payload, "retryable"));
        } catch (IllegalArgumentException exception) {
            throw new TaskCageProtocolException("invalid remote error payload");
        }
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

    public TaskCancellation decodeTaskCancelled(JsonNode response) {
        requireType(response, "taskCancelled");
        JsonNode payload = response.path("payload");
        try {
            return new TaskCancellation(UUID.fromString(requiredText(payload, "taskId")),
                    enumValue(payload, "state", org.taskcage.sdk.TaskState.class),
                    enumValue(payload, "terminationReason", TerminationReason.class));
        } catch (IllegalArgumentException exception) {
            throw new TaskCageProtocolException("invalid taskCancelled response payload", exception);
        }
    }

    public RemoteArtifactChunk decodeArtifactChunk(JsonNode response) {
        requireType(response, "artifactChunk");
        JsonNode payload = response.path("payload");
        try {
            return new RemoteArtifactChunk(UUID.fromString(requiredText(payload, "artifactId")),
                    requiredNonNegativeLong(payload, "offset"),
                    Base64.getDecoder().decode(requiredText(payload, "dataBase64")),
                    requiredNonNegativeLong(payload, "nextOffset"), requiredBoolean(payload, "finished"));
        } catch (IllegalArgumentException exception) {
            throw new TaskCageProtocolException("invalid artifactChunk response payload", exception);
        }
    }

    private FinishedRemoteProfileTaskSnapshot decodeFinishedProfileResult(
            UUID taskId, ProfileIdentity profile, JsonNode payload) {
        ProfileOutcome outcome = enumValue(payload, "profileOutcome", ProfileOutcome.class);
        Map<String, ManagedOutputArtifact> artifacts = new java.util.TreeMap<>();
        JsonNode artifactNodes = requiredObject(payload, "artifacts");
        artifactNodes.properties().forEach(
                entry -> artifacts.put(entry.getKey(), decodeManagedOutputArtifact(entry.getValue())));
        ProfileFailure failure = null;
        if (outcome == ProfileOutcome.FAILED) {
            JsonNode failureNode = requiredObject(payload, "failure");
            failure = new ProfileFailure(
                    requiredText(failureNode, "code"), requiredText(failureNode, "message"));
        } else if (payload.has("failure")) {
            throw new IllegalArgumentException("successful result must not contain failure");
        }
        return new FinishedRemoteProfileTaskSnapshot(
                taskId, profile, outcome, decodeExecutionResult(payload), artifacts, failure);
    }

    private static ProfileIdentity decodeProfile(JsonNode value) {
        return new ProfileIdentity(requiredText(value, "name"), requiredText(value, "version"));
    }

    private static ResourceBudget decodeEffectiveResources(JsonNode value) {
        JsonNode limits = requiredObject(value, "limits");
        JsonNode output = requiredObject(value, "output");
        JsonNode cpuMax = requiredObject(limits, "cpuMax");
        return new ResourceBudget(
                new CpuQuota(
                        requiredPositiveLong(cpuMax, "quotaMicros"),
                        requiredPositiveLong(cpuMax, "periodMicros")),
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
                new ProcessResult(nullableInt(process, "exitCode"), nullableText(process, "signal")),
                new TaskTiming(
                        Instant.parse(requiredText(timing, "submittedAt")),
                        Instant.parse(requiredText(timing, "startedAt")),
                        Instant.parse(requiredText(timing, "finishedAt")),
                        java.time.Duration.ofMillis(requiredNonNegativeLong(timing, "wallTimeMs"))),
                new TaskUsage(
                        requiredNonNegativeLong(usage, "cpuTimeMicros"),
                        requiredNonNegativeLong(usage, "memoryPeakBytes")),
                new TaskOutput(
                        requiredString(output, "stdoutTail"),
                        requiredString(output, "stderrTail"),
                        requiredBoolean(output, "stdoutTruncated"),
                        requiredBoolean(output, "stderrTruncated")));
    }

    private static void requireType(JsonNode response, String type) {
        if (!type.equals(response.path("type").asText())) {
            throw new TaskCageProtocolException("expected " + type + " response");
        }
    }
}
