package org.taskcage.sdk;

import java.util.Objects;
import java.util.regex.Pattern;

/** Immutable identity of an installed Capsule. */
public record CapsuleIdentity(String name, String version) {
    private static final Pattern NAME = Pattern.compile("[a-z][a-z0-9-]{0,62}");
    private static final Pattern VERSION = Pattern.compile("(?:0|[1-9][0-9]*)\\.(?:0|[1-9][0-9]*)\\.(?:0|[1-9][0-9]*)");

    public CapsuleIdentity {
        Objects.requireNonNull(name, "name");
        Objects.requireNonNull(version, "version");
        if (!NAME.matcher(name).matches()) {
            throw new IllegalArgumentException("Capsule name must match [a-z][a-z0-9-]{0,62}");
        }
        if (!VERSION.matcher(version).matches()) {
            throw new IllegalArgumentException("Capsule version must be MAJOR.MINOR.PATCH");
        }
    }
}
