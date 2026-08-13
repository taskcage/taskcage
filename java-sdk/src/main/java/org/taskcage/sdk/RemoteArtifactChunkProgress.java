package org.taskcage.sdk;

import java.util.Objects;
import java.util.UUID;

/** A chunk acknowledgement containing the next safe offset for the same managed upload. */
public record RemoteArtifactChunkProgress(UUID artifactId, long nextOffset) {
    public RemoteArtifactChunkProgress {
        Objects.requireNonNull(artifactId, "artifactId");
        if (nextOffset < 0) {
            throw new IllegalArgumentException("nextOffset must not be negative");
        }
    }
}
