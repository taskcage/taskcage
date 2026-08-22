package org.taskcage.sdk;

import java.io.IOException;
import java.time.Duration;
import java.util.Objects;
import java.util.UUID;
import java.util.concurrent.TimeoutException;

/** Daemon-backed Capsule runner for authenticated TLS Remote Runtimes. */
public final class RemoteCapsuleRunner {
    private final RemoteTaskCageClient client;

    private RemoteCapsuleRunner(RemoteTaskCageClient client) {
        this.client = client;
    }

    /** Adapts a caller-owned Remote client without taking ownership of its connection lifecycle. */
    public static RemoteCapsuleRunner external(RemoteTaskCageClient client) {
        return new RemoteCapsuleRunner(Objects.requireNonNull(client, "client"));
    }

    /** Submits a Capsule and returns a handle for cancellation and terminal observation. */
    public RemoteCapsuleTaskHandle submit(RemoteCapsuleRequest request) {
        return submit(UUID.randomUUID(), request);
    }

    /** Submits a Capsule with a caller-owned idempotency key. */
    public RemoteCapsuleTaskHandle submit(UUID clientRequestId, RemoteCapsuleRequest request) {
        Objects.requireNonNull(clientRequestId, "clientRequestId");
        Objects.requireNonNull(request, "request");
        RemoteProfileTask task = client.submitProfile(clientRequestId, request.profileRequest());
        return new RemoteCapsuleTaskHandle(client, request.capsule(), task.taskId());
    }

    /** Executes a Capsule and waits for its cleanup-confirmed terminal result. */
    public RemoteCapsuleExecutionResult execute(RemoteCapsuleRequest request, Duration waitTimeout)
            throws InterruptedException, TimeoutException {
        Objects.requireNonNull(request, "request");
        TaskHandle.requirePositiveNanos(waitTimeout, "waitTimeout");
        return submit(request).await(waitTimeout);
    }

    /** Executes a Capsule with a caller-owned idempotency key. */
    public RemoteCapsuleExecutionResult execute(
            UUID clientRequestId, RemoteCapsuleRequest request, Duration waitTimeout)
            throws InterruptedException, TimeoutException {
        Objects.requireNonNull(clientRequestId, "clientRequestId");
        Objects.requireNonNull(request, "request");
        TaskHandle.requirePositiveNanos(waitTimeout, "waitTimeout");
        return submit(clientRequestId, request).await(waitTimeout);
    }

    /**
     * Uploads a caller-owned input file, executes the Capsule, and downloads its declared output on success.
     *
     * <p>Transfer and execution use this runner's existing authenticated TLS client. Local file paths never cross
     * the daemon boundary; the daemon sees only its managed input Artifact reference.
     */
    public RemoteCapsuleExecutionResult execute(RemoteCapsuleFileRequest request, Duration waitTimeout)
            throws IOException, InterruptedException, TimeoutException {
        Objects.requireNonNull(request, "request");
        TaskHandle.requirePositiveNanos(waitTimeout, "waitTimeout");
        RemoteArtifactUpload upload = client.upload(request.inputFile(), request.inputMediaType());
        java.util.Map<String, RemoteProfileInputValue> inputs = new java.util.TreeMap<>(request.inputs());
        inputs.put(request.inputSlot(), upload.asInput());
        RemoteCapsuleExecutionResult result = execute(new RemoteCapsuleRequest(
                request.capsule(),
                new RemoteProfileRequest(
                        new ProfileIdentity(request.capsule().name(), request.capsule().version()),
                        inputs,
                        request.resourceOverrides())), waitTimeout);
        if (result.outcome() == ProfileOutcome.SUCCEEDED) {
            ManagedOutputArtifact output = result.profileTask().artifacts().get(request.outputSlot());
            if (output == null) {
                throw new TaskCageProtocolException(
                        "successful Capsule result did not contain output Artifact " + request.outputSlot());
            }
            client.download(output, request.outputFile());
        }
        return result;
    }
}
