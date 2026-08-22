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
     * Uploads the file input with a caller-owned idempotency key and returns its recovery receipt.
     *
     * <p>If the upload response is lost, retry this stage with the same {@code clientArtifactId} and unchanged
     * file. After this method returns, retain the receipt and do not upload again when recovering submission.
     */
    public RemoteArtifactUpload upload(UUID clientArtifactId, RemoteCapsuleFileRequest request)
            throws IOException {
        Objects.requireNonNull(clientArtifactId, "clientArtifactId");
        Objects.requireNonNull(request, "request");
        return client.upload(clientArtifactId, request.inputFile(), request.inputMediaType());
    }

    /**
     * Submits an uploaded file Capsule with a caller-owned idempotency key.
     *
     * <p>If the submission response is lost, retry this stage with the same {@code clientRequestId}, file
     * request, and upload receipt. Do not upload the input again: accepted inputs become task-owned while the
     * daemon retains submission idempotency independently.
     */
    public RemoteCapsuleTaskHandle submit(
            UUID clientRequestId, RemoteCapsuleFileRequest request, RemoteArtifactUpload upload) {
        Objects.requireNonNull(clientRequestId, "clientRequestId");
        Objects.requireNonNull(request, "request");
        Objects.requireNonNull(upload, "upload");
        return submit(clientRequestId, capsuleRequest(request, upload));
    }

    /**
     * Downloads a successful file Capsule output without uploading or submitting a Task.
     *
     * <p>A failed download may be retried with the same terminal result while its output Artifact is retained.
     */
    public void download(RemoteCapsuleFileRequest request, RemoteCapsuleExecutionResult result)
            throws IOException {
        Objects.requireNonNull(request, "request");
        Objects.requireNonNull(result, "result");
        if (!request.capsule().equals(result.capsule())) {
            throw new IllegalArgumentException("result Capsule does not match the file request");
        }
        if (result.outcome() != ProfileOutcome.SUCCEEDED) {
            throw new IllegalStateException("cannot download output from an unsuccessful Capsule result");
        }
        ManagedOutputArtifact output = result.profileTask().artifacts().get(request.outputSlot());
        if (output == null) {
            throw new TaskCageProtocolException(
                    "successful Capsule result did not contain output Artifact " + request.outputSlot());
        }
        client.download(output, request.outputFile());
    }

    /**
     * Uploads a caller-owned input file, executes the Capsule, and downloads its declared output on success.
     *
     * <p>Transfer and execution use this runner's existing authenticated TLS client. Local file paths never cross
     * the daemon boundary; the daemon sees only its managed input Artifact reference.
     *
     * <p>This is a one-shot convenience method with internally generated idempotency keys. Use {@link
     * #upload(UUID, RemoteCapsuleFileRequest)}, {@link #submit(UUID, RemoteCapsuleFileRequest,
     * RemoteArtifactUpload)}, and {@link #download(RemoteCapsuleFileRequest, RemoteCapsuleExecutionResult)} when
     * response-loss recovery is required.
     */
    public RemoteCapsuleExecutionResult execute(RemoteCapsuleFileRequest request, Duration waitTimeout)
            throws IOException, InterruptedException, TimeoutException {
        Objects.requireNonNull(request, "request");
        TaskHandle.requirePositiveNanos(waitTimeout, "waitTimeout");
        RemoteArtifactUpload upload = upload(UUID.randomUUID(), request);
        RemoteCapsuleExecutionResult result = submit(UUID.randomUUID(), request, upload).await(waitTimeout);
        if (result.outcome() == ProfileOutcome.SUCCEEDED) {
            download(request, result);
        }
        return result;
    }

    private static RemoteCapsuleRequest capsuleRequest(
            RemoteCapsuleFileRequest request, RemoteArtifactUpload upload) {
        java.util.Map<String, RemoteProfileInputValue> inputs = new java.util.TreeMap<>(request.inputs());
        inputs.put(request.inputSlot(), upload.asInput());
        return new RemoteCapsuleRequest(
                request.capsule(),
                new RemoteProfileRequest(
                        new ProfileIdentity(request.capsule().name(), request.capsule().version()),
                        inputs,
                        request.resourceOverrides()));
    }
}
