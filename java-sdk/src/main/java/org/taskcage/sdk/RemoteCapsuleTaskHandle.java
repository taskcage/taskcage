package org.taskcage.sdk;

import java.time.Duration;
import java.util.Objects;
import java.util.UUID;
import java.util.concurrent.TimeoutException;

/** Client-bound handle for an accepted Remote Capsule Task. */
public final class RemoteCapsuleTaskHandle {
    private static final Duration DEFAULT_POLL_INTERVAL = Duration.ofMillis(50);

    private final RemoteTaskCageClient client;
    private final CapsuleIdentity capsule;
    private final UUID taskId;
    private volatile RemoteCapsuleExecutionResult finished;

    RemoteCapsuleTaskHandle(RemoteTaskCageClient client, CapsuleIdentity capsule, UUID taskId) {
        this.client = Objects.requireNonNull(client, "client");
        this.capsule = Objects.requireNonNull(capsule, "capsule");
        this.taskId = Objects.requireNonNull(taskId, "taskId");
    }

    public UUID taskId() {
        return taskId;
    }

    /** Requests cancellation; use {@link #await(Duration)} to observe the terminal result. */
    public TaskCancellation cancel() {
        return client.cancelTask(taskId);
    }

    /** Waits without cancelling the Task when the caller's deadline expires. */
    public RemoteCapsuleExecutionResult await(Duration waitTimeout)
            throws InterruptedException, TimeoutException {
        Objects.requireNonNull(waitTimeout, "waitTimeout");
        if (waitTimeout.isZero() || waitTimeout.isNegative()) {
            throw new IllegalArgumentException("waitTimeout must be positive");
        }
        RemoteCapsuleExecutionResult cached = finished;
        if (cached != null) {
            return cached;
        }

        long deadline = System.nanoTime() + waitTimeout.toNanos();
        while (true) {
            if (Thread.interrupted()) {
                throw new InterruptedException();
            }
            RemoteProfileTaskSnapshot snapshot = client.getProfileResult(taskId);
            if (snapshot instanceof FinishedRemoteProfileTaskSnapshot terminal) {
                RemoteCapsuleExecutionResult result = new RemoteCapsuleExecutionResult(capsule, terminal);
                finished = result;
                return result;
            }

            long remainingNanos = deadline - System.nanoTime();
            if (remainingNanos <= 0) {
                throw new TimeoutException("Remote Capsule Task " + taskId + " did not finish before the wait timeout");
            }
            long sleepMillis = Math.min(
                    DEFAULT_POLL_INTERVAL.toMillis(),
                    Math.max(1, Duration.ofNanos(remainingNanos).toMillis()));
            Thread.sleep(sleepMillis);
        }
    }
}
