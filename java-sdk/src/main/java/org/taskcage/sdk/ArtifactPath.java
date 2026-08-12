package org.taskcage.sdk;

import java.nio.charset.StandardCharsets;
import java.util.Objects;

/** A validated UTF-8 path relative to the daemon-configured Local Artifact root. */
public record ArtifactPath(String value) {
    public ArtifactPath {
        Objects.requireNonNull(value, "value");
        int byteLength = value.getBytes(StandardCharsets.UTF_8).length;
        if (byteLength < 1 || byteLength > 4_096) {
            throw new IllegalArgumentException("artifact path must contain 1 to 4096 UTF-8 bytes");
        }
        String[] segments = value.split("/", -1);
        for (String segment : segments) {
            if (segment.isEmpty() || segment.equals(".") || segment.equals("..")) {
                throw new IllegalArgumentException("artifact path cannot contain empty, . or .. segments");
            }
            for (int index = 0; index < segment.length(); index++) {
                char character = segment.charAt(index);
                if (character == '\\' || character <= 0x1f || character == 0x7f) {
                    throw new IllegalArgumentException(
                            "artifact path cannot contain backslash, NUL or ASCII control characters");
                }
            }
        }
        if (segments[0].equals(".taskcage")) {
            throw new IllegalArgumentException("artifact path cannot address the .taskcage staging subtree");
        }
    }

    @Override
    public String toString() {
        return value;
    }
}
