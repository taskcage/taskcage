package org.taskcage.sdk;

import java.time.Instant;
import java.util.Objects;
import java.util.UUID;

/** A daemon-owned Remote output Artifact that can be downloaded before its expiry. */
public record ManagedOutputArtifact(
        UUID artifactId, Sha256Digest digest, long sizeBytes, String mediaType, Instant expiresAt) {
    public ManagedOutputArtifact {
        Objects.requireNonNull(artifactId, "artifactId");
        Objects.requireNonNull(digest, "digest");
        Objects.requireNonNull(mediaType, "mediaType");
        Objects.requireNonNull(expiresAt, "expiresAt");
        if (sizeBytes < 0) {
            throw new IllegalArgumentException("sizeBytes must not be negative");
        }
        if (mediaType.isBlank()) {
            throw new IllegalArgumentException("mediaType must not be blank");
        }
    }
}
