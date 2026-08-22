package org.taskcage.sdk;

import java.util.Objects;
import java.util.regex.Pattern;

/**
 * Immutable identity of an installed Execution Profile.
 *
 * <p>Names use the same dot-separated naming contract as {@link CapsuleIdentity}.
 */
public record ProfileIdentity(String name, String version) {
    private static final Pattern VERSION = Pattern.compile(
            "(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)");

    public ProfileIdentity {
        Objects.requireNonNull(name, "name");
        Objects.requireNonNull(version, "version");
        if (!IdentityNames.isValidCapsuleName(name)) {
            throw new IllegalArgumentException(
                    "profile name must use dot-separated [a-z][a-z0-9-]* segments (maximum 63 bytes)");
        }
        if (!VERSION.matcher(version).matches()) {
            throw new IllegalArgumentException("profile version must be strict MAJOR.MINOR.PATCH");
        }
    }
}
