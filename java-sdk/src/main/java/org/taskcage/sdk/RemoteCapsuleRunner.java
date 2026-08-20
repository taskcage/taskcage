package org.taskcage.sdk;

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
        return submit(request).await(waitTimeout);
    }

    /** Executes a Capsule with a caller-owned idempotency key. */
    public RemoteCapsuleExecutionResult execute(
            UUID clientRequestId, RemoteCapsuleRequest request, Duration waitTimeout)
            throws InterruptedException, TimeoutException {
        return submit(clientRequestId, request).await(waitTimeout);
    }
}
