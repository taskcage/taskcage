package org.taskcage.sdk;

import java.time.Instant;
import java.util.Objects;
import java.util.UUID;

/**
 * A completed, single-use input upload that may be referenced by a {@link ManagedInputArtifact}.
 *
 * <p>When a Profile is accepted, the daemon transfers the Artifact to that Task and releases the upload record.
 * The same client artifact id may then identify a fresh upload, but this Artifact ID cannot be submitted again.
 */
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
