package org.taskcage.sdk;

import java.util.Objects;

/** A cleanup-confirmed Local Artifact published by a successful Profile Task. */
public record PublishedArtifact(
        ArtifactPath path, Sha256Digest digest, long sizeBytes, String mediaType) {
    public PublishedArtifact {
        Objects.requireNonNull(path, "path");
        Objects.requireNonNull(digest, "digest");
        Objects.requireNonNull(mediaType, "mediaType");
        if (sizeBytes < 0) {
            throw new IllegalArgumentException("sizeBytes must not be negative");
        }
        if (mediaType.isBlank()) {
            throw new IllegalArgumentException("mediaType must not be blank");
        }
    }
}
