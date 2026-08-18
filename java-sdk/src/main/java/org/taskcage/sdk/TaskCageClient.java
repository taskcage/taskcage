package org.taskcage.sdk;

import org.taskcage.sdk.internal.client.DefaultTaskCageClient;
import java.nio.file.Path;
import java.time.Duration;
import java.util.Objects;
import java.util.UUID;
import java.util.concurrent.TimeoutException;

/** A client for the TaskCage daemon running on the local Linux host. */
public interface TaskCageClient extends AutoCloseable {
    /** Standard Unix domain socket path used by a packaged local {@code taskcaged} service. */
    Path DEFAULT_SOCKET_PATH = Path.of("/run/taskcage/taskcaged.sock");

    /**
     * Connects to the packaged daemon's standard Unix domain socket.
     *
     * <p>This method does not install or start a daemon. The socket is opened lazily on the first
     * request, and a missing or inaccessible daemon is reported as {@link TaskCageConnectionException}.
     */
    static TaskCageClient localDefault() {
        return connectUnixSocket(DEFAULT_SOCKET_PATH);
    }

    /**
     * Connects to a daemon listening on a Unix domain socket with the SDK's default timeouts.
     *
     * <p>Use {@link #connect(TaskCageClientConfig)} when connection or request timeouts need to
     * be customized.
     */
    static TaskCageClient connectUnixSocket(Path socketPath) {
        return connect(TaskCageClientConfig.builder().socketPath(socketPath).build());
    }

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
     * Submits an installed Local Execution Profile.
     *
     * <p>The default preserves compatibility for custom Protocol v1 client implementations.
     * Built-in clients override it when Protocol v2 is supported by the daemon.
     */
    default ProfileTaskSubmission submitProfile(ProfileRequest request) {
        Objects.requireNonNull(request, "request");
        return submitProfile(UUID.randomUUID(), request);
    }

    /** Submits a Profile Task with a caller-owned idempotency key. */
    default ProfileTaskSubmission submitProfile(UUID clientRequestId, ProfileRequest request) {
        Objects.requireNonNull(clientRequestId, "clientRequestId");
        Objects.requireNonNull(request, "request");
        throw new UnsupportedOperationException("this TaskCageClient does not support Local Profile Protocol v2");
    }

    /** Submits a Profile Task and returns a handle for observation, waiting, and cancellation. */
    default ProfileTaskHandle submitProfileHandle(ProfileRequest request) {
        return ProfileTaskHandle.from(this, submitProfile(request));
    }

    /** Submits a handled Profile Task with a caller-owned idempotency key. */
    default ProfileTaskHandle submitProfileHandle(UUID clientRequestId, ProfileRequest request) {
        return ProfileTaskHandle.from(this, submitProfile(clientRequestId, request));
    }

    /**
     * Submits a Profile Task and waits for cleanup-confirmed completion.
     *
     * <p>A wait timeout never cancels the daemon Task.
     */
    default FinishedProfileTaskSnapshot run(ProfileRequest request, Duration waitTimeout)
            throws InterruptedException, TimeoutException {
        Objects.requireNonNull(request, "request");
        TaskHandle.requirePositiveNanos(waitTimeout, "waitTimeout");
        TaskHandle.throwIfInterrupted(null);
        try {
            return submitProfileHandle(request).await(waitTimeout);
        } catch (TaskCageConnectionException exception) {
            TaskHandle.throwIfInterrupted(exception);
            throw exception;
        }
    }

    /** Submits a Profile Task with a caller-owned idempotency key and waits for completion. */
    default FinishedProfileTaskSnapshot run(
            UUID clientRequestId, ProfileRequest request, Duration waitTimeout)
            throws InterruptedException, TimeoutException {
        Objects.requireNonNull(clientRequestId, "clientRequestId");
        Objects.requireNonNull(request, "request");
        TaskHandle.requirePositiveNanos(waitTimeout, "waitTimeout");
        TaskHandle.throwIfInterrupted(null);
        try {
            return submitProfileHandle(clientRequestId, request).await(waitTimeout);
        } catch (TaskCageConnectionException exception) {
            TaskHandle.throwIfInterrupted(exception);
            throw exception;
        }
    }

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

    /** Returns the Profile-specific current snapshot for a Profile Task. */
    default ProfileTaskSnapshot getProfileResult(UUID taskId) {
        Objects.requireNonNull(taskId, "taskId");
        throw new UnsupportedOperationException("this TaskCageClient does not support Local Profile Protocol v2");
    }

    /**
     * Requests a Profile snapshot with a caller-supplied transport timeout.
     *
     * <p>Custom clients remain compatible through this default implementation.
     */
    default ProfileTaskSnapshot getProfileResult(UUID taskId, Duration requestTimeout) {
        Objects.requireNonNull(taskId, "taskId");
        TaskHandle.requirePositiveNanos(requestTimeout, "requestTimeout");
        return getProfileResult(taskId);
    }

    /** Cancels a running task and returns after daemon cleanup has completed. */
    TaskCancellation cancelTask(UUID taskId);

    /** Closes client-owned transport resources; it never cancels daemon tasks. */
    @Override
    void close();
}
