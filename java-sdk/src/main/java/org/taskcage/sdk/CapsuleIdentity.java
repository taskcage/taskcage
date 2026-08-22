package org.taskcage.sdk;

import java.util.Objects;
import java.util.regex.Pattern;

/**
 * Immutable identity of an installed Capsule.
 *
 * <p>Names contain 1 to 63 ASCII bytes in dot-separated {@code [a-z][a-z0-9-]*} segments.
 */
public record CapsuleIdentity(String name, String version) {
    private static final Pattern VERSION = Pattern.compile("(?:0|[1-9][0-9]*)\\.(?:0|[1-9][0-9]*)\\.(?:0|[1-9][0-9]*)");

    public CapsuleIdentity {
        Objects.requireNonNull(name, "name");
        Objects.requireNonNull(version, "version");
        if (!IdentityNames.isValidCapsuleName(name)) {
            throw new IllegalArgumentException(
                    "Capsule name must use dot-separated [a-z][a-z0-9-]* segments (maximum 63 bytes)");
        }
        if (!VERSION.matcher(version).matches()) {
            throw new IllegalArgumentException("Capsule version must be MAJOR.MINOR.PATCH");
        }
    }
}
