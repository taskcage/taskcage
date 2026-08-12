package org.taskcage.sdk;

import java.util.Objects;

/** A UTF-8 string Profile input whose domain constraints are enforced by the installed Profile. */
public record StringProfileInput(String value) implements ProfileInputValue {
    public StringProfileInput {
        Objects.requireNonNull(value, "value");
    }
}
