package io.github.taskcage.sdk;

import io.github.taskcage.sdk.internal.client.DefaultTaskCageClient;
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

    /** Returns the daemon's current immutable snapshot for a submitted task. */
    TaskSnapshot getTask(UUID taskId);

    /** Cancels a running task and returns after daemon cleanup has completed. */
    TaskCancellation cancelTask(UUID taskId);

    /** Closes client-owned transport resources; it never cancels daemon tasks. */
    @Override
    void close();
}
