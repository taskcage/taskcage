package org.taskcage.sdk;

import java.util.Objects;
import java.util.regex.Pattern;

/** Immutable identity of an installed Execution Profile. */
public record ProfileIdentity(String name, String version) {
    private static final Pattern NAME = Pattern.compile("[a-z][a-z0-9-]{0,62}");
    private static final Pattern VERSION = Pattern.compile(
            "(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)");

    public ProfileIdentity {
        Objects.requireNonNull(name, "name");
        Objects.requireNonNull(version, "version");
        if (!NAME.matcher(name).matches()) {
            throw new IllegalArgumentException("profile name must match [a-z][a-z0-9-]{0,62}");
        }
        if (!VERSION.matcher(version).matches()) {
            throw new IllegalArgumentException("profile version must be strict MAJOR.MINOR.PATCH");
        }
    }
}
