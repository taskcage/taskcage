package org.taskcage.sdk.internal.remote;

import org.taskcage.sdk.RemoteConnectionOptions;
import org.taskcage.sdk.RemoteTaskCageClient;

/** Service provider that keeps public factories independent of internal TLS client classes. */
public final class DefaultRemoteTaskCageClientProvider implements RemoteTaskCageClient.Provider {
    @Override
    public RemoteTaskCageClient connect(RemoteConnectionOptions options) {
        return new DefaultRemoteTaskCageClient(options);
    }
}
