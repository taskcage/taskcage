package org.taskcage.sdk;

import java.io.IOException;
import java.net.URI;
import java.nio.file.Path;
import java.time.Duration;
import java.util.UUID;
import org.taskcage.sdk.internal.remote.DefaultRemoteTaskCageClient;

/** Authenticated TLS client for Profile-only execution on a Remote TaskCage daemon. */
public interface RemoteTaskCageClient extends AutoCloseable {
    /**
     * Connects to a TLS-protected Remote Runtime using the platform's default trust configuration.
     *
     * <p>{@code endpoint} must use the {@code taskcage+tls://host:port} form. Use
     * {@link #connect(RemoteConnectionOptions)} for custom TLS trust or timeout configuration.
     */
    static RemoteTaskCageClient connect(URI endpoint, ServiceCredentials credentials) {
        return connect(RemoteConnectionOptions.builder(endpoint, credentials).build());
    }

    static RemoteTaskCageClient connect(RemoteConnectionOptions options) {
        return new DefaultRemoteTaskCageClient(options);
    }

    RemoteCapabilities capabilities();

    RemoteArtifactUpload upload(Path source, String mediaType) throws IOException;

    RemoteArtifactUpload upload(UUID clientArtifactId, Path source, String mediaType) throws IOException;

    /**
     * Downloads and verifies an Artifact before atomically replacing the destination.
     *
     * <p>The built-in client uses a unique, exclusively created temporary file in the destination
     * directory and removes it unless the atomic move succeeds.
     */
    void download(ManagedOutputArtifact artifact, Path destination) throws IOException;

    RemoteProfileTask submitProfile(RemoteProfileRequest request);

    RemoteProfileTask submitProfile(UUID clientRequestId, RemoteProfileRequest request);

    RemoteProfileTaskSnapshot getProfileResult(UUID taskId);

    /**
     * Requests a Remote Profile snapshot with a caller-supplied transport timeout.
     *
     * <p>The default implementation preserves compatibility for custom clients. The built-in TLS
     * client applies this timeout to connection setup, authentication, and response reads.
     */
    default RemoteProfileTaskSnapshot getProfileResult(UUID taskId, Duration requestTimeout) {
        TaskHandle.requirePositiveNanos(requestTimeout, "requestTimeout");
        return getProfileResult(taskId);
    }

    TaskCancellation cancelTask(UUID taskId);

    @Override
    void close();
}
