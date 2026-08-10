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
import io.github.taskcage.sdk.TaskSubmission;
import io.github.taskcage.sdk.ResourceBudget;
import io.github.taskcage.sdk.ExecutionResult;
import io.github.taskcage.sdk.FinishedTaskSnapshot;
import io.github.taskcage.sdk.ProcessResult;
import io.github.taskcage.sdk.RunningTaskSnapshot;
import io.github.taskcage.sdk.TaskCageDaemonException;
import io.github.taskcage.sdk.TaskCancellation;
import io.github.taskcage.sdk.TaskOutput;
import io.github.taskcage.sdk.TaskSnapshot;
import io.github.taskcage.sdk.TaskState;
import io.github.taskcage.sdk.TaskTiming;
import io.github.taskcage.sdk.TaskUsage;
import io.github.taskcage.sdk.TerminationReason;
import io.github.taskcage.sdk.internal.transport.UnixDomainSocketConnection;
import java.io.IOException;
import java.time.Duration;
import java.time.Instant;
import java.util.ArrayList;
import java.util.List;
import java.util.UUID;
import java.util.concurrent.TimeUnit;
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
        limits.put("wallTimeLimitMs", budget.wallTimeLimit().toMillis());
        ObjectNode output = payload.putObject("output");
        output.put("stdoutTailMaxBytes", budget.stdoutTailMaxBytes());
        output.put("stderrTailMaxBytes", budget.stderrTailMaxBytes());
        return decodeSubmission(request("submitTask", payload), budget);
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
        long startedAt = System.nanoTime();
        long totalTimeoutNanos = totalTimeout == null ? 0 : requirePositiveNanos(totalTimeout, "requestTimeout");
        lockForRequest(totalTimeoutNanos);
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
            validateResponse(response, requestId);
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

    private static String requiredString(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || !value.isTextual()) {
            throw new IllegalArgumentException(field + " must be a string");
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
