package org.taskcage.sdk.internal.client;

import org.taskcage.sdk.TaskCageClient;
import org.taskcage.sdk.TaskCageClientConfig;

/** Service provider that keeps public factories independent of internal client classes. */
public final class DefaultTaskCageClientProvider implements TaskCageClient.Provider {
    @Override
    public TaskCageClient connect(TaskCageClientConfig config) {
        return new DefaultTaskCageClient(config);
    }
}
