package org.taskcage.sdk;

import java.time.Duration;
import java.util.Objects;
import java.util.UUID;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.TimeoutException;

/** A client-bound handle for one submitted Profile Task. */
public final class ProfileTaskHandle {
    private static final Duration DEFAULT_POLL_INTERVAL = Duration.ofMillis(100);

    private final TaskCageClient client;
    private final UUID taskId;
    private volatile FinishedProfileTaskSnapshot finishedSnapshot;

    private ProfileTaskHandle(
            TaskCageClient client, UUID taskId, FinishedProfileTaskSnapshot finishedSnapshot) {
        this.client = Objects.requireNonNull(client, "client");
        this.taskId = Objects.requireNonNull(taskId, "taskId");
        this.finishedSnapshot = finishedSnapshot;
    }

    static ProfileTaskHandle from(TaskCageClient client, ProfileTaskSubmission submission) {
        Objects.requireNonNull(submission, "submission");
        if (submission instanceof FinishedProfileTaskSnapshot finished) {
            return new ProfileTaskHandle(client, finished.taskId(), finished);
        }
        ProfileTask task = (ProfileTask) submission;
        return new ProfileTaskHandle(client, task.taskId(), null);
    }

    public UUID taskId() {
        return taskId;
    }

    /** Returns the current Profile snapshot, reusing an observed terminal result. */
    public ProfileTaskSnapshot get() {
        FinishedProfileTaskSnapshot cached = finishedSnapshot;
        if (cached != null) {
            return cached;
        }
        return rememberFinished(client.getProfileResult(taskId));
    }

    /** Waits with a 100 millisecond polling interval. */
    public FinishedProfileTaskSnapshot await(Duration timeout)
            throws InterruptedException, TimeoutException {
        return await(timeout, DEFAULT_POLL_INTERVAL);
    }

    /** Waits without cancelling the Profile Task when the caller's deadline expires. */
    public FinishedProfileTaskSnapshot await(Duration timeout, Duration pollInterval)
            throws InterruptedException, TimeoutException {
        long timeoutNanos = TaskHandle.requirePositiveNanos(timeout, "timeout");
        long pollNanos = TaskHandle.requirePositiveNanos(pollInterval, "pollInterval");
        FinishedProfileTaskSnapshot cached = finishedSnapshot;
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

            ProfileTaskSnapshot snapshot;
            try {
                snapshot = rememberFinished(
                        client.getProfileResult(taskId, Duration.ofNanos(remainingNanos)));
            } catch (TaskCageConnectionException exception) {
                TaskHandle.throwIfInterrupted(exception);
                if (remainingNanos(startedAt, timeoutNanos) <= 0) {
                    throw timeoutException(exception);
                }
                throw exception;
            }
            if (snapshot instanceof FinishedProfileTaskSnapshot finished) {
                return finished;
            }

            remainingNanos = remainingNanos(startedAt, timeoutNanos);
            if (remainingNanos <= 0) {
                throw timeoutException(null);
            }
            try {
                TimeUnit.NANOSECONDS.sleep(Math.min(pollNanos, remainingNanos));
            } catch (InterruptedException exception) {
                Thread.currentThread().interrupt();
                throw exception;
            }
        }
    }

    /** Uses the existing Protocol v1 cancellation operation and waits for cleanup confirmation. */
    public TaskCancellation cancel() {
        return client.cancelTask(taskId);
    }

    private ProfileTaskSnapshot rememberFinished(ProfileTaskSnapshot snapshot) {
        if (snapshot instanceof FinishedProfileTaskSnapshot finished) {
            finishedSnapshot = finished;
        }
        return snapshot;
    }

    private static long remainingNanos(long startedAt, long timeoutNanos) {
        return timeoutNanos - (System.nanoTime() - startedAt);
    }

    private TimeoutException timeoutException(Throwable cause) {
        TimeoutException exception =
                new TimeoutException("Profile Task " + taskId + " did not finish before the wait timeout");
        if (cause != null) {
            exception.initCause(cause);
        }
        return exception;
    }
}
