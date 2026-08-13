package org.taskcage.sdk;

import java.util.Objects;

/** A scalar string Remote Profile input. */
public record RemoteStringInput(String value) implements RemoteProfileInputValue {
    public RemoteStringInput {
        Objects.requireNonNull(value, "value");
    }
}
