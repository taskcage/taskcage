package org.taskcage.sdk;

import java.util.Arrays;
import java.util.Objects;

/** A client secret whose value is intentionally omitted from {@link #toString()}. */
public final class Secret {
    private final char[] characters;

    private Secret(char[] characters) {
        this.characters = characters;
    }

    /** Creates a secret from an application-supplied value. */
    public static Secret of(String value) {
        Objects.requireNonNull(value, "value");
        if (value.isEmpty() || value.length() > 4096) {
            throw new IllegalArgumentException("secret must contain between 1 and 4096 characters");
        }
        return new Secret(value.toCharArray());
    }

    /** Reads a required secret from an environment variable. */
    public static Secret fromEnvironment(String variableName) {
        Objects.requireNonNull(variableName, "variableName");
        String value = System.getenv(variableName);
        if (value == null || value.isEmpty()) {
            throw new IllegalArgumentException("environment variable " + variableName + " must contain a secret");
        }
        return of(value);
    }

    /** Returns a short-lived copy for an authenticated transport request. */
    public char[] copyCharacters() {
        return characters.clone();
    }

    /** Overwrites this instance's character storage when the application no longer needs it. */
    public void clear() {
        Arrays.fill(characters, '\0');
    }

    @Override
    public String toString() {
        return "[secret]";
    }
}
