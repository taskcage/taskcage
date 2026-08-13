package org.taskcage.sdk;

import java.util.List;
import java.util.Objects;

/** Remote capabilities observed after TLS service-account authentication. */
public record RemoteCapabilities(
        String daemonVersion,
        List<Integer> remoteProtocolVersions,
        int maxFrameBytes,
        List<String> artifactModes,
        long maxArtifactBytes,
        int maxArtifactChunkBytes,
        long artifactRetentionSeconds) {
    public RemoteCapabilities {
        Objects.requireNonNull(daemonVersion, "daemonVersion");
        remoteProtocolVersions = List.copyOf(Objects.requireNonNull(remoteProtocolVersions, "remoteProtocolVersions"));
        artifactModes = List.copyOf(Objects.requireNonNull(artifactModes, "artifactModes"));
        if (daemonVersion.isBlank()
                || maxFrameBytes <= 0
                || maxArtifactBytes <= 0
                || maxArtifactChunkBytes <= 0
                || artifactRetentionSeconds <= 0) {
            throw new IllegalArgumentException("remote capability limits must be positive");
        }
    }

    /** Returns whether this daemon supports the managed upload/download mode required by this SDK. */
    public boolean supportsManagedTransfer() {
        return remoteProtocolVersions.contains(1) && artifactModes.contains("MANAGED_TRANSFER");
    }
}
