package org.taskcage.sdk.internal.client;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.node.ObjectNode;
import java.time.Duration;
import java.util.UUID;
import org.taskcage.sdk.ProfileRequest;
import org.taskcage.sdk.ProfileTaskSnapshot;
import org.taskcage.sdk.ProfileTaskSubmission;
import org.taskcage.sdk.TaskCageCapabilities;
import org.taskcage.sdk.TaskCageClient;
import org.taskcage.sdk.TaskCageClientConfig;
import org.taskcage.sdk.TaskCageConnectionException;
import org.taskcage.sdk.TaskCageProtocolException;
import org.taskcage.sdk.TaskCancellation;
import org.taskcage.sdk.TaskSnapshot;
import org.taskcage.sdk.TaskSpec;
import org.taskcage.sdk.TaskSubmission;
import org.taskcage.sdk.internal.protocol.local.LocalRequestEncoder;
import org.taskcage.sdk.internal.protocol.local.LocalResponseDecoder;

/** Dispatches public client operations to the Local Protocol implementation. */
public final class DefaultTaskCageClient implements TaskCageClient {
    private static final int PROTOCOL_V1 = 1;
    private static final int PROTOCOL_V2 = 2;

    private final LocalRequestEncoder requestEncoder = new LocalRequestEncoder();
    private final LocalResponseDecoder responseDecoder = new LocalResponseDecoder();
    private final LocalRequestExecutor requestExecutor;
    private volatile boolean profileProtocolConfirmed;

    public DefaultTaskCageClient(TaskCageClientConfig config) {
        LocalConnectionManager connectionManager = new LocalConnectionManager(config);
        requestExecutor = new LocalRequestExecutor(connectionManager, requestEncoder, responseDecoder);
    }

    @Override
    public TaskCageCapabilities capabilities() {
        TaskCageConnectionException lastFailure = null;
        for (int attempt = 0; attempt < 2; attempt++) {
            try {
                TaskCageCapabilities capabilities = responseDecoder.decodeCapabilities(request("getCapabilities"));
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
        ObjectNode payload = requestEncoder.submitTaskPayload(clientRequestId, task);
        return responseDecoder.decodeSubmission(request("submitTask", payload), task.budget());
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
        ObjectNode payload = requestEncoder.profilePayload(clientRequestId, request);
        return responseDecoder.decodeProfileSubmission(
                request(PROTOCOL_V2, "submitProfile", payload), request.profile());
    }

    @Override
    public TaskSnapshot getTask(UUID taskId) {
        return requestTask(taskId, null);
    }

    @Override
    public TaskSnapshot getTask(UUID taskId, Duration requestTimeout) {
        LocalRequestExecutor.requirePositiveNanos(requestTimeout, "requestTimeout");
        return requestTask(taskId, requestTimeout);
    }

    private TaskSnapshot requestTask(UUID taskId, Duration requestTimeout) {
        if (taskId == null) {
            throw new NullPointerException("taskId");
        }
        ObjectNode payload = requestEncoder.taskIdPayload(taskId);
        JsonNode response = requestTimeout == null
                ? request("getTask", payload)
                : request("getTask", payload, requestTimeout);
        return responseDecoder.decodeTask(response, taskId);
    }

    @Override
    public ProfileTaskSnapshot getProfileResult(UUID taskId) {
        return requestProfileResult(taskId, null);
    }

    @Override
    public ProfileTaskSnapshot getProfileResult(UUID taskId, Duration requestTimeout) {
        LocalRequestExecutor.requirePositiveNanos(requestTimeout, "requestTimeout");
        return requestProfileResult(taskId, requestTimeout);
    }

    private ProfileTaskSnapshot requestProfileResult(UUID taskId, Duration requestTimeout) {
        if (taskId == null) {
            throw new NullPointerException("taskId");
        }
        long startedAt = System.nanoTime();
        long totalTimeoutNanos = requestTimeout == null
                ? 0
                : LocalRequestExecutor.requirePositiveNanos(requestTimeout, "requestTimeout");
        if (requestTimeout == null) {
            requireProfileProtocol();
        } else {
            requireProfileProtocol(requestTimeout);
        }
        ObjectNode payload = requestEncoder.taskIdPayload(taskId);
        JsonNode response = requestTimeout == null
                ? request(PROTOCOL_V2, "getProfileResult", payload)
                : request(
                        PROTOCOL_V2,
                        "getProfileResult",
                        payload,
                        LocalRequestExecutor.remainingRequestDuration(startedAt, totalTimeoutNanos));
        return responseDecoder.decodeProfileResult(response, taskId, null);
    }

    @Override
    public TaskCancellation cancelTask(UUID taskId) {
        if (taskId == null) {
            throw new NullPointerException("taskId");
        }
        ObjectNode payload = requestEncoder.taskIdPayload(taskId);
        return responseDecoder.decodeCancellation(request("cancelTask", payload), taskId);
    }

    @Override
    public void close() {
        requestExecutor.close();
    }

    private JsonNode request(String type) {
        return request(type, requestEncoder.emptyPayload());
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
        return requestExecutor.request(protocolVersion, type, payload, totalTimeout);
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
        TaskCageCapabilities capabilities = responseDecoder.decodeCapabilities(request(
                PROTOCOL_V1, "getCapabilities", requestEncoder.emptyPayload(), requestTimeout));
        if (!capabilities.protocolVersions().contains(PROTOCOL_V2)) {
            throw new TaskCageProtocolException("TaskCage daemon does not support Local Profile Protocol v2");
        }
        profileProtocolConfirmed = true;
    }
}
