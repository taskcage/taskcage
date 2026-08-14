package org.taskcage.sdk;

import java.util.Objects;
import java.util.UUID;

/** One ordered byte range read from a managed Remote output Artifact. */
public record RemoteArtifactChunk(UUID artifactId, long offset, byte[] bytes, long nextOffset, boolean finished) {
    public RemoteArtifactChunk {
        Objects.requireNonNull(artifactId, "artifactId");
        Objects.requireNonNull(bytes, "bytes");
        bytes = bytes.clone();
        if (offset < 0 || nextOffset != offset + bytes.length) {
            throw new IllegalArgumentException("chunk offsets must match its byte length");
        }
    }

    @Override
    public byte[] bytes() {
        return bytes.clone();
    }
}
