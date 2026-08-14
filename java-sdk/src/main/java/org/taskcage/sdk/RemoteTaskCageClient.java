package org.taskcage.sdk;

import java.io.IOException;
import java.nio.file.Path;
import java.util.UUID;
import org.taskcage.sdk.internal.remote.DefaultRemoteTaskCageClient;

/** Authenticated TLS client for Profile-only execution on a Remote TaskCage daemon. */
public interface RemoteTaskCageClient extends AutoCloseable {
    static RemoteTaskCageClient connect(RemoteConnectionOptions options) {
        return new DefaultRemoteTaskCageClient(options);
    }

    RemoteCapabilities capabilities();

    RemoteArtifactUpload upload(Path source, String mediaType) throws IOException;

    RemoteArtifactUpload upload(UUID clientArtifactId, Path source, String mediaType) throws IOException;

    void download(ManagedOutputArtifact artifact, Path destination) throws IOException;

    RemoteProfileTask submitProfile(RemoteProfileRequest request);

    RemoteProfileTask submitProfile(UUID clientRequestId, RemoteProfileRequest request);

    RemoteProfileTaskSnapshot getProfileResult(UUID taskId);

    TaskCancellation cancelTask(UUID taskId);

    @Override
    void close();
}
