package org.taskcage.sdk;

import java.util.Collections;
import java.util.Map;
import java.util.Objects;
import java.util.TreeMap;
import java.util.regex.Pattern;

/** Immutable Profile request for an authenticated Remote Runtime. */
public record RemoteProfileRequest(
        ProfileIdentity profile,
        Map<String, RemoteProfileInputValue> inputs,
        ProfileResourceOverrides resourceOverrides) {
    private static final Pattern SLOT_NAME = Pattern.compile("[a-z][a-z0-9_-]{0,63}");

    public RemoteProfileRequest(ProfileIdentity profile, Map<String, RemoteProfileInputValue> inputs) {
        this(profile, inputs, ProfileResourceOverrides.none());
    }

    public RemoteProfileRequest {
        Objects.requireNonNull(profile, "profile");
        Objects.requireNonNull(inputs, "inputs");
        Objects.requireNonNull(resourceOverrides, "resourceOverrides");
        if (inputs.isEmpty()) {
            throw new IllegalArgumentException("inputs must not be empty");
        }
        TreeMap<String, RemoteProfileInputValue> copy = new TreeMap<>();
        inputs.forEach((name, value) -> {
            Objects.requireNonNull(name, "input slot name");
            Objects.requireNonNull(value, "input value");
            if (!SLOT_NAME.matcher(name).matches()) {
                throw new IllegalArgumentException("input slot name must match [a-z][a-z0-9_-]{0,63}");
            }
            copy.put(name, value);
        });
        inputs = Collections.unmodifiableMap(copy);
    }
}
