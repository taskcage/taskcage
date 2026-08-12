package org.taskcage.sdk.internal.client;

import com.fasterxml.jackson.core.JsonProcessingException;
import com.fasterxml.jackson.core.StreamReadFeature;
import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.json.JsonMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;
import org.taskcage.sdk.ArtifactPath;
import org.taskcage.sdk.BooleanProfileInput;
import org.taskcage.sdk.CpuQuota;
import org.taskcage.sdk.FinishedProfileTaskSnapshot;
import org.taskcage.sdk.Int64ProfileInput;
import org.taskcage.sdk.LocalInputArtifact;
import org.taskcage.sdk.ProfileFailure;
import org.taskcage.sdk.ProfileIdentity;
import org.taskcage.sdk.ProfileInputValue;
import org.taskcage.sdk.ProfileOutcome;
import org.taskcage.sdk.ProfileRequest;
import org.taskcage.sdk.ProfileResourceOverrides;
import org.taskcage.sdk.ProfileTask;
import org.taskcage.sdk.ProfileTaskSnapshot;
import org.taskcage.sdk.ProfileTaskSubmission;
import org.taskcage.sdk.PublishedArtifact;
import org.taskcage.sdk.RunningProfileTaskSnapshot;
import org.taskcage.sdk.Sha256Digest;
import org.taskcage.sdk.StringProfileInput;
import org.taskcage.sdk.TaskCageCapabilities;
import org.taskcage.sdk.TaskCageClient;
import org.taskcage.sdk.TaskCageClientConfig;
import org.taskcage.sdk.TaskCageConnectionException;
import org.taskcage.sdk.TaskCageProtocolException;
import org.taskcage.sdk.Task;
import org.taskcage.sdk.TaskSpec;
import org.taskcage.sdk.TaskSubmission;
import org.taskcage.sdk.ResourceBudget;
import org.taskcage.sdk.ExecutionResult;
import org.taskcage.sdk.FinishedTaskSnapshot;
import org.taskcage.sdk.ProcessResult;
import org.taskcage.sdk.RunningTaskSnapshot;
import org.taskcage.sdk.TaskCageDaemonException;
import org.taskcage.sdk.TaskCancellation;
import org.taskcage.sdk.TaskOutput;
import org.taskcage.sdk.TaskSnapshot;
import org.taskcage.sdk.TaskState;
import org.taskcage.sdk.TaskTiming;
import org.taskcage.sdk.TaskUsage;
import org.taskcage.sdk.TerminationReason;
import org.taskcage.sdk.internal.transport.UnixDomainSocketConnection;
import java.io.IOException;
import java.time.Duration;
import java.time.Instant;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;
import java.util.UUID;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.locks.ReentrantLock;

/** Serializes Protocol v1 requests over one lazily connected Unix domain socket. */
public final class DefaultTaskCageClient implements TaskCageClient {
    private static final int PROTOCOL_V1 = 1;
    private static final int PROTOCOL_V2 = 2;

    private final TaskCageClientConfig config;
    private final ObjectMapper mapper = JsonMapper.builder()
            .enable(StreamReadFeature.STRICT_DUPLICATE_DETECTION)
            .build();
    private final ReentrantLock requestLock = new ReentrantLock();
    private UnixDomainSocketConnection connection;
    private boolean closed;
    private volatile boolean profileProtocolConfirmed;

    public DefaultTaskCageClient(TaskCageClientConfig config) {
        this.config = config;
    }

    @Override
    public TaskCageCapabilities capabilities() {
        TaskCageConnectionException lastFailure = null;
        for (int attempt = 0; attempt < 2; attempt++) {
            try {
                TaskCageCapabilities capabilities = decodeCapabilities(request("getCapabilities"));
                if (capabilities.protocolVersions().contains(PROTOCOL_V2)) {
                    profileProtocolConfirmed = true;
                }
                return capabilities;
            } catch (TaskCageConnectionException exception) {
                lastFailure = exception;
            }
        }
        throw lastFailure;
    }

    @Override
    public TaskSubmission submit(TaskSpec task) {
        return submit(UUID.randomUUID(), task);
    }

    @Override
    public TaskSubmission submit(UUID clientRequestId, TaskSpec task) {
        if (clientRequestId == null) {
            throw new NullPointerException("clientRequestId");
        }
        ObjectNode payload = mapper.createObjectNode();
        payload.put("clientRequestId", clientRequestId.toString());
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
        limits.put("wallTimeLimitMs", budget.wallTimeLimitMillis());
        ObjectNode output = payload.putObject("output");
        output.put("stdoutTailMaxBytes", budget.stdoutTailMaxBytes());
        output.put("stderrTailMaxBytes", budget.stderrTailMaxBytes());
        return decodeSubmission(request("submitTask", payload), budget);
    }

    @Override
    public ProfileTaskSubmission submitProfile(ProfileRequest request) {
        return submitProfile(UUID.randomUUID(), request);
    }

    @Override
    public ProfileTaskSubmission submitProfile(UUID clientRequestId, ProfileRequest request) {
        if (clientRequestId == null) {
            throw new NullPointerException("clientRequestId");
        }
        if (request == null) {
            throw new NullPointerException("request");
        }
        requireProfileProtocol();
        ObjectNode payload = encodeProfileRequest(clientRequestId, request);
        return decodeProfileSubmission(request(PROTOCOL_V2, "submitProfile", payload), request.profile());
    }

    @Override
    public TaskSnapshot getTask(UUID taskId) {
        return requestTask(taskId, null);
    }

    @Override
    public TaskSnapshot getTask(UUID taskId, Duration requestTimeout) {
        requirePositiveNanos(requestTimeout, "requestTimeout");
        return requestTask(taskId, requestTimeout);
    }

    private TaskSnapshot requestTask(UUID taskId, Duration requestTimeout) {
        if (taskId == null) {
            throw new NullPointerException("taskId");
        }
        ObjectNode payload = mapper.createObjectNode();
        payload.put("taskId", taskId.toString());
        JsonNode response = requestTimeout == null
                ? request("getTask", payload)
                : request("getTask", payload, requestTimeout);
        return decodeTask(response, taskId);
    }

    @Override
    public ProfileTaskSnapshot getProfileResult(UUID taskId) {
        return requestProfileResult(taskId, null);
    }

    @Override
    public ProfileTaskSnapshot getProfileResult(UUID taskId, Duration requestTimeout) {
        requirePositiveNanos(requestTimeout, "requestTimeout");
        return requestProfileResult(taskId, requestTimeout);
    }

    private ProfileTaskSnapshot requestProfileResult(UUID taskId, Duration requestTimeout) {
        if (taskId == null) {
            throw new NullPointerException("taskId");
        }
        long startedAt = System.nanoTime();
        long totalTimeoutNanos = requestTimeout == null
                ? 0
                : requirePositiveNanos(requestTimeout, "requestTimeout");
        if (requestTimeout == null) {
            requireProfileProtocol();
        } else {
            requireProfileProtocol(requestTimeout);
        }
        ObjectNode payload = mapper.createObjectNode();
        payload.put("taskId", taskId.toString());
        JsonNode response = requestTimeout == null
                ? request(PROTOCOL_V2, "getProfileResult", payload)
                : request(
                        PROTOCOL_V2,
                        "getProfileResult",
                        payload,
                        remainingRequestDuration(startedAt, totalTimeoutNanos));
        return decodeProfileResult(response, taskId, null);
    }

    @Override
    public TaskCancellation cancelTask(UUID taskId) {
        if (taskId == null) {
            throw new NullPointerException("taskId");
        }
        ObjectNode payload = mapper.createObjectNode();
        payload.put("taskId", taskId.toString());
        return decodeCancellation(request("cancelTask", payload), taskId);
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
        return request(type, payload, null);
    }

    private JsonNode request(String type, ObjectNode payload, Duration totalTimeout) {
        return request(PROTOCOL_V1, type, payload, totalTimeout);
    }

    private JsonNode request(int protocolVersion, String type, ObjectNode payload) {
        return request(protocolVersion, type, payload, null);
    }

    private JsonNode request(int protocolVersion, String type, ObjectNode payload, Duration totalTimeout) {
        long startedAt = System.nanoTime();
        long totalTimeoutNanos = totalTimeout == null ? 0 : requirePositiveNanos(totalTimeout, "requestTimeout");
        lockForRequest(totalTimeoutNanos);
        try {
            if (closed) {
                throw new IllegalStateException("TaskCageClient is closed");
            }
            String requestId = UUID.randomUUID().toString();
            ObjectNode request = mapper.createObjectNode();
            request.put("protocolVersion", protocolVersion);
            request.put("requestId", requestId);
            request.put("type", type);
            request.set("payload", payload);

            UnixDomainSocketConnection activeConnection;
            Duration responseTimeout;
            if (totalTimeout == null) {
                activeConnection = requireConnection();
                responseTimeout = config.requestTimeout();
            } else {
                activeConnection = requireConnectionWithin(remainingDuration(startedAt, totalTimeoutNanos));
                responseTimeout = shorter(config.requestTimeout(), remainingDuration(startedAt, totalTimeoutNanos));
            }
            byte[] responseBytes = activeConnection.request(writeRequest(request), responseTimeout);
            JsonNode response = mapper.readTree(responseBytes);
            validateResponse(response, requestId, protocolVersion);
            if ("error".equals(response.path("type").asText())) {
                throw decodeDaemonError(response);
            }
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

    private void lockForRequest(long totalTimeoutNanos) {
        if (totalTimeoutNanos == 0) {
            requestLock.lock();
            return;
        }
        try {
            if (!requestLock.tryLock(totalTimeoutNanos, TimeUnit.NANOSECONDS)) {
                throw new TaskCageConnectionException(
                        "timed out waiting to send a TaskCage daemon request",
                        new IOException("request lock timeout"));
            }
        } catch (InterruptedException exception) {
            Thread.currentThread().interrupt();
            throw new TaskCageConnectionException(
                    "interrupted while waiting to send a TaskCage daemon request", exception);
        }
    }

    private void requireProfileProtocol() {
        if (profileProtocolConfirmed) {
            return;
        }
        if (!capabilities().protocolVersions().contains(PROTOCOL_V2)) {
            throw new TaskCageProtocolException("TaskCage daemon does not support Local Profile Protocol v2");
        }
        profileProtocolConfirmed = true;
    }

    private void requireProfileProtocol(Duration requestTimeout) {
        if (profileProtocolConfirmed) {
            return;
        }
        TaskCageCapabilities capabilities = decodeCapabilities(request(
                PROTOCOL_V1, "getCapabilities", mapper.createObjectNode(), requestTimeout));
        if (!capabilities.protocolVersions().contains(PROTOCOL_V2)) {
            throw new TaskCageProtocolException("TaskCage daemon does not support Local Profile Protocol v2");
        }
        profileProtocolConfirmed = true;
    }

    private static Duration remainingRequestDuration(long startedAt, long timeoutNanos) {
        long remainingNanos = timeoutNanos - (System.nanoTime() - startedAt);
        if (remainingNanos <= 0) {
            throw new TaskCageConnectionException(
                    "timed out waiting to send a TaskCage daemon request",
                    new IOException("request timeout"));
        }
        return Duration.ofNanos(remainingNanos);
    }

    private ObjectNode encodeProfileRequest(UUID clientRequestId, ProfileRequest request) {
        ObjectNode payload = mapper.createObjectNode();
        payload.put("clientRequestId", clientRequestId.toString());
        encodeProfileIdentity(payload.putObject("profile"), request.profile());
        ObjectNode inputs = payload.putObject("inputs");
        request.inputs().forEach((slot, value) -> encodeProfileInput(inputs.putObject(slot), value));
        if (!request.resourceOverrides().isEmpty()) {
            encodeResourceOverrides(payload.putObject("resourceOverrides"), request.resourceOverrides());
        }
        return payload;
    }

    private static void encodeProfileIdentity(ObjectNode target, ProfileIdentity profile) {
        target.put("name", profile.name());
        target.put("version", profile.version());
    }

    private static void encodeProfileInput(ObjectNode target, ProfileInputValue value) {
        if (value instanceof StringProfileInput string) {
            target.put("kind", "STRING");
            target.put("value", string.value());
        } else if (value instanceof Int64ProfileInput integer) {
            target.put("kind", "INT64");
            target.put("value", integer.value());
        } else if (value instanceof BooleanProfileInput bool) {
            target.put("kind", "BOOLEAN");
            target.put("value", bool.value());
        } else if (value instanceof LocalInputArtifact artifact) {
            target.put("kind", "LOCAL_INPUT");
            target.put("path", artifact.path().value());
            target.put("digest", artifact.digest().value());
            target.put("sizeBytes", artifact.sizeBytes());
        } else {
            throw new TaskCageProtocolException("unsupported Profile input value type");
        }
    }

    private static void encodeResourceOverrides(
            ObjectNode target, ProfileResourceOverrides overrides) {
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

    private ProfileTaskSubmission decodeProfileSubmission(
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

    private ProfileTaskSnapshot decodeProfileResult(
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

    private static void requireMatchingProfile(
            ProfileIdentity actual, ProfileIdentity expected) {
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
                    new org.taskcage.sdk.CpuQuota(requiredPositiveLong(limits.path("cpuMax"), "quotaMicros"), requiredPositiveLong(limits.path("cpuMax"), "periodMicros")),
                    requiredPositiveLong(limits, "memoryMaxBytes"), requiredPositiveLong(limits, "pidsMax"),
                    java.time.Duration.ofMillis(requiredPositiveLong(limits, "wallTimeLimitMs")),
                    requestedBudget.stdoutTailMaxBytes(), requestedBudget.stderrTailMaxBytes()));
        } catch (IllegalArgumentException exception) {
            throw new TaskCageProtocolException("invalid taskAccepted payload", exception);
        }
    }

    private TaskSubmission decodeSubmission(JsonNode response, ResourceBudget requestedBudget) {
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

    private TaskSnapshot decodeTask(JsonNode response, UUID expectedTaskId) {
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

    private TaskCancellation decodeCancellation(JsonNode response, UUID expectedTaskId) {
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

    private TaskCageDaemonException decodeDaemonError(JsonNode response) {
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

    private UnixDomainSocketConnection requireConnection() throws IOException {
        if (connection == null) {
            connection = UnixDomainSocketConnection.connect(config.socketPath(), config.connectTimeout());
        }
        return connection;
    }

    private UnixDomainSocketConnection requireConnectionWithin(Duration timeout) throws IOException {
        if (connection == null) {
            connection = UnixDomainSocketConnection.connect(config.socketPath(), shorter(config.connectTimeout(), timeout));
        }
        return connection;
    }

    private static Duration remainingDuration(long startedAt, long timeoutNanos) throws IOException {
        long remainingNanos = timeoutNanos - (System.nanoTime() - startedAt);
        if (remainingNanos <= 0) {
            throw new IOException("timed out waiting for a TaskCage daemon response");
        }
        return Duration.ofNanos(remainingNanos);
    }

    private static Duration shorter(Duration first, Duration second) {
        return first.compareTo(second) <= 0 ? first : second;
    }

    private static long requirePositiveNanos(Duration duration, String name) {
        if (duration == null) {
            throw new NullPointerException(name);
        }
        try {
            long nanos = duration.toNanos();
            if (nanos <= 0) {
                throw new IllegalArgumentException(name + " must be positive and representable in nanoseconds");
            }
            return nanos;
        } catch (ArithmeticException exception) {
            throw new IllegalArgumentException(name + " must be representable in nanoseconds", exception);
        }
    }

    private byte[] writeRequest(ObjectNode request) {
        try {
            return mapper.writeValueAsBytes(request);
        } catch (JsonProcessingException exception) {
            throw new TaskCageProtocolException("could not encode TaskCage protocol request", exception);
        }
    }

    private void validateResponse(JsonNode response, String requestId, int protocolVersion) {
        if (!response.isObject()
                || response.path("protocolVersion").asInt(-1) != protocolVersion
                || !requestId.equals(response.path("requestId").asText())
                || !response.has("type")
                || !response.path("payload").isObject()) {
            throw new TaskCageProtocolException(
                    "invalid Protocol v" + protocolVersion + " response envelope");
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

    private static String requiredString(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || !value.isTextual()) {
            throw new IllegalArgumentException(field + " must be a string");
        }
        return value.textValue();
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

    private static int requiredInt(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || !value.isIntegralNumber() || !value.canConvertToInt()) {
            throw new IllegalArgumentException(field + " must be an integer");
        }
        return value.intValue();
    }

    private static Integer optionalInt(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || value.isNull()) {
            return null;
        }
        return requiredInt(object, field);
    }

    private static String optionalText(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || value.isNull()) {
            return null;
        }
        return requiredText(object, field);
    }

    private static JsonNode requiredObject(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || !value.isObject()) {
            throw new IllegalArgumentException(field + " must be an object");
        }
        return value;
    }

    private static Instant requiredInstant(JsonNode object, String field) {
        return Instant.parse(requiredText(object, field));
    }

    private static <T extends Enum<T>> T requiredEnum(JsonNode object, String field, Class<T> type) {
        try {
            return Enum.valueOf(type, requiredText(object, field));
        } catch (IllegalArgumentException exception) {
            throw new IllegalArgumentException(field + " must be a supported " + type.getSimpleName(), exception);
        }
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
