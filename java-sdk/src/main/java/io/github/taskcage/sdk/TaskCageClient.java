package io.github.taskcage.sdk;

import io.github.taskcage.sdk.internal.client.DefaultTaskCageClient;
import java.time.Duration;
import java.util.Objects;
import java.util.UUID;

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
