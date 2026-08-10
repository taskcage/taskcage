package org.taskcage.sdk;

import java.time.Duration;
import java.util.Objects;
import java.util.UUID;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.TimeoutException;

/** A client-bound handle for one submitted Task. */
public final class TaskHandle {
    private static final Duration DEFAULT_POLL_INTERVAL = Duration.ofMillis(100);

    private final TaskCageClient client;
    private final UUID taskId;
    private volatile FinishedTaskSnapshot finishedSnapshot;

    private TaskHandle(TaskCageClient client, UUID taskId, FinishedTaskSnapshot finishedSnapshot) {
        this.client = Objects.requireNonNull(client, "client");
        this.taskId = Objects.requireNonNull(taskId, "taskId");
        this.finishedSnapshot = finishedSnapshot;
    }

    static TaskHandle from(TaskCageClient client, TaskSubmission submission) {
        Objects.requireNonNull(submission, "submission");
        if (submission instanceof FinishedTaskSnapshot finished) {
            return new TaskHandle(client, finished.taskId(), finished);
        }
        Task task = (Task) submission;
        return new TaskHandle(client, task.taskId(), null);
    }

    /**
     * Returns the daemon task identifier.
     *
     * @return task identifier
     */
    public UUID taskId() {
        return taskId;
    }

    /**
     * Returns the latest snapshot, reusing an already observed terminal snapshot.
     *
     * @return current immutable task snapshot
     */
    public TaskSnapshot get() {
        FinishedTaskSnapshot cached = finishedSnapshot;
        if (cached != null) {
            return cached;
        }
        return rememberFinished(client.getTask(taskId));
    }

    /**
     * Waits with a 100 millisecond polling interval and a caller-supplied total timeout.
     *
     * @param timeout positive, nanosecond-representable total wait timeout
     * @return the cleanup-confirmed terminal snapshot
     * @throws InterruptedException if the waiting thread is interrupted
     * @throws TimeoutException if the task does not finish within the timeout
     */
    public FinishedTaskSnapshot await(Duration timeout) throws InterruptedException, TimeoutException {
        return await(timeout, DEFAULT_POLL_INTERVAL);
    }

    /**
     * Waits until FINISHED without cancelling the Task when the wait times out.
     *
     * @param timeout positive, nanosecond-representable total wait timeout
     * @param pollInterval positive, nanosecond-representable delay between snapshots
     * @return the cleanup-confirmed terminal snapshot
     * @throws InterruptedException if the waiting thread is interrupted
     * @throws TimeoutException if the task does not finish within the timeout
     */
    public FinishedTaskSnapshot await(Duration timeout, Duration pollInterval)
            throws InterruptedException, TimeoutException {
        long timeoutNanos = requirePositiveNanos(timeout, "timeout");
        long pollNanos = requirePositiveNanos(pollInterval, "pollInterval");
        FinishedTaskSnapshot cached = finishedSnapshot;
        if (cached != null) {
            return cached;
        }

        long startedAt = System.nanoTime();

        while (true) {
            throwIfInterrupted(null);
            long remainingNanos = remainingNanos(startedAt, timeoutNanos);
            if (remainingNanos <= 0) {
                throw timeoutException(null);
            }

            TaskSnapshot snapshot;
            try {
                snapshot = rememberFinished(client.getTask(taskId, Duration.ofNanos(remainingNanos)));
            } catch (TaskCageConnectionException exception) {
                throwIfInterrupted(exception);
                if (remainingNanos(startedAt, timeoutNanos) <= 0) {
                    throw timeoutException(exception);
                }
                throw exception;
            }
            if (snapshot instanceof FinishedTaskSnapshot finished) {
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

    /**
     * Cancels the Task and returns only after the daemon confirms whole-task cleanup.
     *
     * @return cleanup-confirmed cancellation result
     */
    public TaskCancellation cancel() {
        return client.cancelTask(taskId);
    }

    private TaskSnapshot rememberFinished(TaskSnapshot snapshot) {
        if (snapshot instanceof FinishedTaskSnapshot finished) {
            finishedSnapshot = finished;
        }
        return snapshot;
    }

    private long remainingNanos(long startedAt, long timeoutNanos) {
        return timeoutNanos - (System.nanoTime() - startedAt);
    }

    private TimeoutException timeoutException(Throwable cause) {
        TimeoutException exception = new TimeoutException("Task " + taskId + " did not finish before the wait timeout");
        if (cause != null) {
            exception.initCause(cause);
        }
        return exception;
    }

    static long requirePositiveNanos(Duration duration, String name) {
        Objects.requireNonNull(duration, name);
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

    static void throwIfInterrupted(Throwable cause) throws InterruptedException {
        if (!Thread.currentThread().isInterrupted()) {
            return;
        }
        InterruptedException exception = new InterruptedException("interrupted while waiting for a Task");
        if (cause != null) {
            exception.initCause(cause);
        }
        Thread.currentThread().interrupt();
        throw exception;
    }
}
