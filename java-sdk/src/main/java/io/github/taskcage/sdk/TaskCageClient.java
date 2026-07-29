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

    /** Submits a constrained task and returns after the daemon has accepted it. */
    Task submit(TaskSpec task);

    /** Returns the daemon's current immutable snapshot for a submitted task. */
    TaskSnapshot getTask(UUID taskId);

    /** Closes client-owned transport resources; it never cancels daemon tasks. */
    @Override
    void close();
}
