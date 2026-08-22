package org.taskcage.sdk;

import java.time.Duration;
import java.util.Objects;
import java.util.UUID;
import java.util.concurrent.TimeUnit;
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

    /**
     * Waits without cancelling the Task when the caller's deadline expires.
     *
     * <p>The timeout covers polling delays and each built-in TLS snapshot request. A timeout never
     * cancels the accepted Remote Task.
     */
    public RemoteCapsuleExecutionResult await(Duration waitTimeout)
            throws InterruptedException, TimeoutException {
        long timeoutNanos = TaskHandle.requirePositiveNanos(waitTimeout, "waitTimeout");
        RemoteCapsuleExecutionResult cached = finished;
        if (cached != null) {
            return cached;
        }

        long startedAt = System.nanoTime();
        while (true) {
            TaskHandle.throwIfInterrupted(null);
            long remainingNanos = remainingNanos(startedAt, timeoutNanos);
            if (remainingNanos <= 0) {
                throw timeoutException(null);
            }

            RemoteProfileTaskSnapshot snapshot;
            try {
                snapshot = client.getProfileResult(taskId, Duration.ofNanos(remainingNanos));
            } catch (TaskCageConnectionException exception) {
                TaskHandle.throwIfInterrupted(exception);
                if (remainingNanos(startedAt, timeoutNanos) <= 0) {
                    throw timeoutException(exception);
                }
                throw exception;
            }
            if (snapshot instanceof FinishedRemoteProfileTaskSnapshot terminal) {
                RemoteCapsuleExecutionResult result = new RemoteCapsuleExecutionResult(capsule, terminal);
                finished = result;
                return result;
            }

            remainingNanos = remainingNanos(startedAt, timeoutNanos);
            if (remainingNanos <= 0) {
                throw timeoutException(null);
            }
            try {
                TimeUnit.NANOSECONDS.sleep(
                        Math.min(DEFAULT_POLL_INTERVAL.toNanos(), remainingNanos));
            } catch (InterruptedException exception) {
                Thread.currentThread().interrupt();
                throw exception;
            }
        }
    }

    private static long remainingNanos(long startedAt, long timeoutNanos) {
        return timeoutNanos - (System.nanoTime() - startedAt);
    }

    private TimeoutException timeoutException(Throwable cause) {
        TimeoutException exception = new TimeoutException(
                "Remote Capsule Task " + taskId + " did not finish before the wait timeout");
        if (cause != null) {
            exception.initCause(cause);
        }
        return exception;
    }
}
