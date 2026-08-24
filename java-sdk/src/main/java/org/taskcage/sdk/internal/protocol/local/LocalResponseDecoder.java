package org.taskcage.sdk.internal.protocol.local;

import static org.taskcage.sdk.internal.protocol.common.JsonFields.optionalInt;
import static org.taskcage.sdk.internal.protocol.common.JsonFields.optionalText;
import static org.taskcage.sdk.internal.protocol.common.JsonFields.requiredBoolean;
import static org.taskcage.sdk.internal.protocol.common.JsonFields.requiredEnum;
import static org.taskcage.sdk.internal.protocol.common.JsonFields.requiredInstant;
import static org.taskcage.sdk.internal.protocol.common.JsonFields.requiredNonNegativeLong;
import static org.taskcage.sdk.internal.protocol.common.JsonFields.requiredObject;
import static org.taskcage.sdk.internal.protocol.common.JsonFields.requiredPositiveInt;
import static org.taskcage.sdk.internal.protocol.common.JsonFields.requiredPositiveLong;
import static org.taskcage.sdk.internal.protocol.common.JsonFields.requiredString;
import static org.taskcage.sdk.internal.protocol.common.JsonFields.requiredText;

import com.fasterxml.jackson.core.StreamReadFeature;
import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.json.JsonMapper;
import java.io.IOException;
import java.time.Duration;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;
import java.util.UUID;
import org.taskcage.sdk.ArtifactPath;
import org.taskcage.sdk.CpuQuota;
import org.taskcage.sdk.ExecutionResult;
import org.taskcage.sdk.FinishedProfileTaskSnapshot;
import org.taskcage.sdk.FinishedTaskSnapshot;
import org.taskcage.sdk.ProcessResult;
import org.taskcage.sdk.ProfileFailure;
import org.taskcage.sdk.ProfileIdentity;
import org.taskcage.sdk.ProfileOutcome;
import org.taskcage.sdk.ProfileTask;
import org.taskcage.sdk.ProfileTaskSnapshot;
import org.taskcage.sdk.ProfileTaskSubmission;
import org.taskcage.sdk.PublishedArtifact;
import org.taskcage.sdk.ResourceBudget;
import org.taskcage.sdk.RunningProfileTaskSnapshot;
import org.taskcage.sdk.RunningTaskSnapshot;
import org.taskcage.sdk.Sha256Digest;
import org.taskcage.sdk.Task;
import org.taskcage.sdk.TaskCageCapabilities;
import org.taskcage.sdk.TaskCageDaemonException;
import org.taskcage.sdk.TaskCageProtocolException;
import org.taskcage.sdk.TaskCancellation;
import org.taskcage.sdk.TaskOutput;
import org.taskcage.sdk.TaskSnapshot;
import org.taskcage.sdk.TaskState;
import org.taskcage.sdk.TaskSubmission;
import org.taskcage.sdk.TaskTiming;
import org.taskcage.sdk.TaskUsage;
import org.taskcage.sdk.TerminationReason;

/** Decodes Local Protocol response envelopes and payloads. */
public final class LocalResponseDecoder {
    private final ObjectMapper mapper = JsonMapper.builder()
            .enable(StreamReadFeature.STRICT_DUPLICATE_DETECTION)
            .build();

    public JsonNode read(byte[] responseBytes) throws IOException {
        return mapper.readTree(responseBytes);
    }

    public void validateEnvelope(JsonNode response, UUID requestId, int protocolVersion) {
        if (!response.isObject()
                || response.path("protocolVersion").asInt(-1) != protocolVersion
                || !requestId.toString().equals(response.path("requestId").asText())
                || !response.has("type")
                || !response.path("payload").isObject()) {
            throw new TaskCageProtocolException(
                    "invalid Protocol v" + protocolVersion + " response envelope");
        }
    }

    public ProfileTaskSubmission decodeProfileSubmission(
            JsonNode response, ProfileIdentity expectedProfile) {
        String responseType = response.path("type").asText();
        if ("profileAccepted".equals(responseType)) {
            return decodeProfileAccepted(response, expectedProfile);
        }
        if ("profileResult".equals(responseType)) {
            ProfileTaskSnapshot snapshot = decodeProfileResult(response, null, expectedProfile);
            if (snapshot instanceof FinishedProfileTaskSnapshot finished) {
                return finished;
            }
        }
        throw new TaskCageProtocolException("expected profileAccepted or finished profileResult response");
    }

    public ProfileTaskSnapshot decodeProfileResult(
            JsonNode response, UUID expectedTaskId, ProfileIdentity expectedProfile) {
        if (!"profileResult".equals(response.path("type").asText())) {
            throw new TaskCageProtocolException("expected profileResult response");
        }
        JsonNode payload = response.path("payload");
        try {
            UUID taskId = UUID.fromString(requiredText(payload, "taskId"));
            if (expectedTaskId != null && !expectedTaskId.equals(taskId)) {
                throw new IllegalArgumentException("taskId does not match the requested Profile Task");
            }
            ProfileIdentity profile = decodeProfileIdentity(requiredObject(payload, "profile"));
            requireMatchingProfile(profile, expectedProfile);
            return switch (requiredText(payload, "state")) {
                case "RUNNING" -> new RunningProfileTaskSnapshot(
                        taskId,
                        profile,
                        requiredInstant(payload, "submittedAt"),
                        requiredInstant(payload, "startedAt"));
                case "FINISHED" -> decodeFinishedProfileResult(taskId, profile, payload);
                default -> throw new IllegalArgumentException("state must be RUNNING or FINISHED");
            };
        } catch (IllegalArgumentException exception) {
            throw new TaskCageProtocolException("invalid profileResult payload", exception);
        }
    }

    public TaskSubmission decodeSubmission(JsonNode response, ResourceBudget requestedBudget) {
        String responseType = response.path("type").asText();
        if ("taskAccepted".equals(responseType)) {
            return decodeAccepted(response, requestedBudget);
        }
        if ("task".equals(responseType)) {
            TaskSnapshot snapshot = decodeTask(response, null);
            if (snapshot instanceof FinishedTaskSnapshot finished) {
                return finished;
            }
        }
        throw new TaskCageProtocolException("expected taskAccepted or finished task response");
    }

    public TaskSnapshot decodeTask(JsonNode response, UUID expectedTaskId) {
        if (!"task".equals(response.path("type").asText())) {
            throw new TaskCageProtocolException("expected task response");
        }
        JsonNode payload = response.path("payload");
        try {
            UUID taskId = UUID.fromString(requiredText(payload, "taskId"));
            if (expectedTaskId != null && !expectedTaskId.equals(taskId)) {
                throw new IllegalArgumentException("taskId does not match the requested task");
            }
            return switch (requiredText(payload, "state")) {
                case "RUNNING" -> new RunningTaskSnapshot(
                        taskId,
                        requiredInstant(payload, "submittedAt"),
                        requiredInstant(payload, "startedAt"));
                case "FINISHED" -> new FinishedTaskSnapshot(taskId, decodeExecutionResult(payload));
                default -> throw new IllegalArgumentException("state must be RUNNING or FINISHED");
            };
        } catch (IllegalArgumentException exception) {
            throw new TaskCageProtocolException("invalid task payload", exception);
        }
    }

    public TaskCancellation decodeCancellation(JsonNode response, UUID expectedTaskId) {
        if (!"taskCancelled".equals(response.path("type").asText())) {
            throw new TaskCageProtocolException("expected taskCancelled response");
        }
        JsonNode payload = response.path("payload");
        try {
            UUID taskId = UUID.fromString(requiredText(payload, "taskId"));
            if (!expectedTaskId.equals(taskId)) {
                throw new IllegalArgumentException("taskId does not match the requested task");
            }
            return new TaskCancellation(
                    taskId,
                    requiredEnum(payload, "state", TaskState.class),
                    requiredEnum(payload, "terminationReason", TerminationReason.class));
        } catch (IllegalArgumentException exception) {
            throw new TaskCageProtocolException("invalid taskCancelled payload", exception);
        }
    }

    public TaskCageDaemonException decodeDaemonError(JsonNode response) {
        JsonNode payload = response.path("payload");
        try {
            return new TaskCageDaemonException(
                    requiredText(payload, "code"),
                    requiredText(payload, "message"),
                    requiredBoolean(payload, "retryable"));
        } catch (IllegalArgumentException exception) {
            throw new TaskCageProtocolException("invalid error payload", exception);
        }
    }

    public TaskCageCapabilities decodeCapabilities(JsonNode response) {
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

    private ProfileTask decodeProfileAccepted(JsonNode response, ProfileIdentity expectedProfile) {
        JsonNode payload = response.path("payload");
        if (!"RUNNING".equals(payload.path("state").asText())) {
            throw new TaskCageProtocolException("expected a RUNNING profileAccepted response");
        }
        try {
            ProfileIdentity profile = decodeProfileIdentity(requiredObject(payload, "profile"));
            requireMatchingProfile(profile, expectedProfile);
            return new ProfileTask(
                    UUID.fromString(requiredText(payload, "taskId")),
                    profile,
                    decodeEffectiveResources(requiredObject(payload, "effectiveResources")));
        } catch (IllegalArgumentException exception) {
            throw new TaskCageProtocolException("invalid profileAccepted payload", exception);
        }
    }

    private FinishedProfileTaskSnapshot decodeFinishedProfileResult(
            UUID taskId, ProfileIdentity profile, JsonNode payload) {
        ProfileOutcome outcome = requiredEnum(payload, "profileOutcome", ProfileOutcome.class);
        Map<String, PublishedArtifact> artifacts = decodePublishedArtifacts(requiredObject(payload, "artifacts"));
        ProfileFailure failure = null;
        if (outcome == ProfileOutcome.FAILED) {
            JsonNode failureNode = requiredObject(payload, "failure");
            failure = new ProfileFailure(
                    requiredText(failureNode, "code"), requiredText(failureNode, "message"));
        } else if (payload.has("failure")) {
            throw new IllegalArgumentException("successful profileResult must not contain failure");
        }
        return new FinishedProfileTaskSnapshot(
                taskId, profile, outcome, decodeExecutionResult(payload), artifacts, failure);
    }

    private static ProfileIdentity decodeProfileIdentity(JsonNode value) {
        return new ProfileIdentity(requiredText(value, "name"), requiredText(value, "version"));
    }

    private static void requireMatchingProfile(ProfileIdentity actual, ProfileIdentity expected) {
        if (expected != null && !expected.equals(actual)) {
            throw new IllegalArgumentException("profile identity does not match the submitted Profile");
        }
    }

    private static ResourceBudget decodeEffectiveResources(JsonNode resources) {
        JsonNode limits = requiredObject(resources, "limits");
        JsonNode output = requiredObject(resources, "output");
        JsonNode cpuMax = requiredObject(limits, "cpuMax");
        return new ResourceBudget(
                new CpuQuota(
                        requiredPositiveLong(cpuMax, "quotaMicros"),
                        requiredPositiveLong(cpuMax, "periodMicros")),
                requiredPositiveLong(limits, "memoryMaxBytes"),
                requiredPositiveLong(limits, "pidsMax"),
                Duration.ofMillis(requiredPositiveLong(limits, "wallTimeLimitMs")),
                requiredPositiveInt(output, "stdoutTailMaxBytes"),
                requiredPositiveInt(output, "stderrTailMaxBytes"));
    }

    private static Map<String, PublishedArtifact> decodePublishedArtifacts(JsonNode value) {
        Map<String, PublishedArtifact> artifacts = new TreeMap<>();
        var fields = value.properties().iterator();
        while (fields.hasNext()) {
            Map.Entry<String, JsonNode> entry = fields.next();
            JsonNode artifact = entry.getValue();
            if (!artifact.isObject() || !"LOCAL_FILE".equals(requiredText(artifact, "kind"))) {
                throw new IllegalArgumentException("published Artifact must have kind LOCAL_FILE");
            }
            artifacts.put(entry.getKey(), new PublishedArtifact(
                    new ArtifactPath(requiredText(artifact, "path")),
                    new Sha256Digest(requiredText(artifact, "digest")),
                    requiredNonNegativeLong(artifact, "sizeBytes"),
                    requiredText(artifact, "mediaType")));
        }
        return artifacts;
    }

    private Task decodeAccepted(JsonNode response, ResourceBudget requestedBudget) {
        if (!"taskAccepted".equals(response.path("type").asText())
                || !"RUNNING".equals(response.path("payload").path("state").asText())) {
            throw new TaskCageProtocolException("expected taskAccepted response");
        }
        JsonNode limits = response.path("payload").path("effectiveLimits");
        try {
            return new Task(UUID.fromString(requiredText(response.path("payload"), "taskId")), new ResourceBudget(
                    new CpuQuota(requiredPositiveLong(limits.path("cpuMax"), "quotaMicros"), requiredPositiveLong(limits.path("cpuMax"), "periodMicros")),
                    requiredPositiveLong(limits, "memoryMaxBytes"), requiredPositiveLong(limits, "pidsMax"),
                    Duration.ofMillis(requiredPositiveLong(limits, "wallTimeLimitMs")),
                    requestedBudget.stdoutTailMaxBytes(), requestedBudget.stderrTailMaxBytes()));
        } catch (IllegalArgumentException exception) {
            throw new TaskCageProtocolException("invalid taskAccepted payload", exception);
        }
    }

    private ExecutionResult decodeExecutionResult(JsonNode payload) {
        JsonNode process = requiredObject(payload, "process");
        JsonNode timing = requiredObject(payload, "timing");
        JsonNode usage = requiredObject(payload, "usage");
        JsonNode output = requiredObject(payload, "output");
        return new ExecutionResult(
                requiredEnum(payload, "terminationReason", TerminationReason.class),
                new ProcessResult(optionalInt(process, "exitCode"), optionalText(process, "signal")),
                new TaskTiming(
                        requiredInstant(timing, "submittedAt"),
                        requiredInstant(timing, "startedAt"),
                        requiredInstant(timing, "finishedAt"),
                        Duration.ofMillis(requiredNonNegativeLong(timing, "wallTimeMs"))),
                new TaskUsage(
                        requiredNonNegativeLong(usage, "cpuTimeMicros"),
                        requiredNonNegativeLong(usage, "memoryPeakBytes")),
                new TaskOutput(
                        requiredString(output, "stdoutTail"),
                        requiredString(output, "stderrTail"),
                        requiredBoolean(output, "stdoutTruncated"),
                        requiredBoolean(output, "stderrTruncated")));
    }
}
