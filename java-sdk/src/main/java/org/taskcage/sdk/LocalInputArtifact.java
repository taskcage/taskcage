package org.taskcage.sdk;

import java.util.Objects;

/** An immutable descriptor for one caller-owned file under the Local Artifact root. */
public record LocalInputArtifact(ArtifactPath path, Sha256Digest digest, long sizeBytes)
        implements ProfileInputValue {
    public LocalInputArtifact {
        Objects.requireNonNull(path, "path");
        Objects.requireNonNull(digest, "digest");
        if (sizeBytes < 0) {
            throw new IllegalArgumentException("sizeBytes must not be negative");
        }
    }
}
