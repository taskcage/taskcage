package org.taskcage.sdk;

import java.util.Objects;
import java.util.UUID;

/** The daemon's idempotent begin-upload result, including the exact safe resume offset. */
public record RemoteArtifactUploadStart(
        UUID artifactId, RemoteArtifactUploadState state, long nextOffset) {
    public RemoteArtifactUploadStart {
        Objects.requireNonNull(artifactId, "artifactId");
        Objects.requireNonNull(state, "state");
        if (nextOffset < 0) {
            throw new IllegalArgumentException("nextOffset must not be negative");
        }
    }
}
