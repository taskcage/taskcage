package org.taskcage.sdk;

import org.taskcage.sdk.internal.client.DefaultTaskCageClient;
import java.time.Duration;
import java.util.Objects;
import java.util.UUID;
import java.util.concurrent.TimeoutException;

/** A client for the TaskCage daemon running on the local Linux host. */
public interface TaskCageClient extends AutoCloseable {
    /**
     * Creates a client. The Unix domain socket is opened lazily on the first request.
     */
    static TaskCageClient connect(TaskCageClientConfig config) {
        return new DefaultTaskCageClient(config);
    }

    /** Returns the daemon capabilities after a request-response round trip. */
    TaskCageCapabilities capabilities();

    /**
     * Submits a constrained task. The daemon returns {@link Task} when it accepts the task or a
     * {@link FinishedTaskSnapshot} when execution could not be started and cleanup has completed.
     */
    TaskSubmission submit(TaskSpec task);

    /** Submits with a caller-owned idempotency key that can be reused after a lost response. */
    TaskSubmission submit(UUID clientRequestId, TaskSpec task);

    /**
     * Submits a task and returns a handle that can query, await, or cancel it.
     *
     * @param task task contract to submit
     * @return handle bound to the accepted or immediately finished task
     */
    default TaskHandle submitHandle(TaskSpec task) {
        return TaskHandle.from(this, submit(task));
    }

    /**
     * Submits a handled task with a caller-owned idempotency key for lost-response recovery.
     *
     * @param clientRequestId caller-owned idempotency key
     * @param task task contract to submit
     * @return handle bound to the accepted or immediately finished task
     */
    default TaskHandle submitHandle(UUID clientRequestId, TaskSpec task) {
        return TaskHandle.from(this, submit(clientRequestId, task));
    }

    /**
     * Submits a task and waits for its cleanup-confirmed terminal snapshot.
     *
     * <p>The wait timeout starts after submission completes and does not cancel the task when it
     * expires. Use a caller-owned idempotency key when a lost response or wait timeout must be
     * recovered later.
     *
     * @param task task contract to submit
     * @param waitTimeout positive, nanosecond-representable completion wait timeout
     * @return cleanup-confirmed terminal snapshot
     * @throws InterruptedException if the calling thread is interrupted
     * @throws TimeoutException if the task does not finish within the wait timeout
     */
    default FinishedTaskSnapshot run(TaskSpec task, Duration waitTimeout)
            throws InterruptedException, TimeoutException {
        Objects.requireNonNull(task, "task");
        TaskHandle.requirePositiveNanos(waitTimeout, "waitTimeout");
        TaskHandle.throwIfInterrupted(null);
        try {
            return submitHandle(task).await(waitTimeout);
        } catch (TaskCageConnectionException exception) {
            TaskHandle.throwIfInterrupted(exception);
            throw exception;
        }
    }

    /**
     * Submits with a caller-owned idempotency key and waits for terminal cleanup.
     *
     * <p>The wait timeout starts after submission completes and does not cancel the task when it
     * expires. Reuse {@code clientRequestId} with {@link #submitHandle(UUID, TaskSpec)} to recover
     * the task after a lost response or wait timeout.
     *
     * @param clientRequestId caller-owned idempotency key
     * @param task task contract to submit
     * @param waitTimeout positive, nanosecond-representable completion wait timeout
     * @return cleanup-confirmed terminal snapshot
     * @throws InterruptedException if the calling thread is interrupted
     * @throws TimeoutException if the task does not finish within the wait timeout
     */
    default FinishedTaskSnapshot run(UUID clientRequestId, TaskSpec task, Duration waitTimeout)
            throws InterruptedException, TimeoutException {
        Objects.requireNonNull(clientRequestId, "clientRequestId");
        Objects.requireNonNull(task, "task");
        TaskHandle.requirePositiveNanos(waitTimeout, "waitTimeout");
        TaskHandle.throwIfInterrupted(null);
        try {
            return submitHandle(clientRequestId, task).await(waitTimeout);
        } catch (TaskCageConnectionException exception) {
            TaskHandle.throwIfInterrupted(exception);
            throw exception;
        }
    }

    /** Returns the daemon's current immutable snapshot for a submitted task. */
    TaskSnapshot getTask(UUID taskId);

    /**
     * Requests a snapshot with a caller-supplied timeout.
     *
     * <p>The built-in client bounds lock acquisition, connection, and response I/O. Custom client
     * implementations remain source compatible through this default implementation and may override
     * it to provide the same transport-level bound.
     *
     * @param taskId daemon task identifier
     * @param requestTimeout positive, nanosecond-representable request timeout
     * @return current immutable task snapshot
     */
    default TaskSnapshot getTask(UUID taskId, Duration requestTimeout) {
        Objects.requireNonNull(requestTimeout, "requestTimeout");
        try {
            if (requestTimeout.toNanos() <= 0) {
                throw new IllegalArgumentException("requestTimeout must be positive and representable in nanoseconds");
            }
        } catch (ArithmeticException exception) {
            throw new IllegalArgumentException("requestTimeout must be representable in nanoseconds", exception);
        }
        return getTask(taskId);
    }

    /** Cancels a running task and returns after daemon cleanup has completed. */
    TaskCancellation cancelTask(UUID taskId);

    /** Closes client-owned transport resources; it never cancels daemon tasks. */
    @Override
    void close();
}
