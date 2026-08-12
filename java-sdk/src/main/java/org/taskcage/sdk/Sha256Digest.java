package org.taskcage.sdk;

import java.util.Objects;
import java.util.regex.Pattern;

/** A canonical lowercase SHA-256 identity used by Profile Artifacts. */
public record Sha256Digest(String value) {
    private static final Pattern CANONICAL = Pattern.compile("sha256:[0-9a-f]{64}");

    public Sha256Digest {
        Objects.requireNonNull(value, "value");
        if (!CANONICAL.matcher(value).matches()) {
            throw new IllegalArgumentException("digest must be sha256: followed by 64 lowercase hexadecimal digits");
        }
    }

    @Override
    public String toString() {
        return value;
    }
}
