package org.taskcage.sdk;

import java.time.Instant;
import java.util.Objects;
import java.util.UUID;

/** A completed input upload that may be referenced by a {@link ManagedInputArtifact}. */
public record RemoteArtifactUpload(UUID artifactId, Sha256Digest digest, long sizeBytes, Instant expiresAt) {
    public RemoteArtifactUpload {
        Objects.requireNonNull(artifactId, "artifactId");
        Objects.requireNonNull(digest, "digest");
        Objects.requireNonNull(expiresAt, "expiresAt");
        if (sizeBytes <= 0) {
            throw new IllegalArgumentException("sizeBytes must be positive");
        }
    }

    public ManagedInputArtifact asInput() {
        return new ManagedInputArtifact(artifactId);
    }
}
