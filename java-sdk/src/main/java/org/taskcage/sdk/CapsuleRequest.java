package org.taskcage.sdk;

import java.util.Map;
import java.util.Objects;

/** Immutable typed inputs for one exact Capsule execution. */
public record CapsuleRequest(
        CapsuleIdentity capsule,
        Map<String, ProfileInputValue> inputs,
        ProfileResourceOverrides resourceOverrides) {
    public CapsuleRequest(CapsuleIdentity capsule, Map<String, ProfileInputValue> inputs) {
        this(capsule, inputs, ProfileResourceOverrides.none());
    }

    public CapsuleRequest {
        Objects.requireNonNull(capsule, "capsule");
        ProfileRequest validated = new ProfileRequest(
                new ProfileIdentity(capsule.name(), capsule.version()),
                Objects.requireNonNull(inputs, "inputs"),
                Objects.requireNonNull(resourceOverrides, "resourceOverrides"));
        inputs = validated.inputs();
        resourceOverrides = validated.resourceOverrides();
    }

    /** Starts a concise request for one exact Capsule identity. */
    public static Builder builder(String name, String version) {
        return builder(new CapsuleIdentity(name, version));
    }

    /** Starts a concise request for an existing Capsule identity. */
    public static Builder builder(CapsuleIdentity capsule) {
        return new Builder(capsule);
    }

    /** Converts this Capsule-level request only at the daemon-backed Profile adapter boundary. */
    ProfileRequest toProfileRequest() {
        return new ProfileRequest(
                new ProfileIdentity(capsule.name(), capsule.version()), inputs, resourceOverrides);
    }

    /** Builder that keeps Profile identity and wire input wrappers out of ordinary Capsule calls. */
    public static final class Builder {
        private final CapsuleIdentity capsule;
        private final java.util.LinkedHashMap<String, ProfileInputValue> inputs =
                new java.util.LinkedHashMap<>();
        private ProfileResourceOverrides resourceOverrides = ProfileResourceOverrides.none();

        private Builder(CapsuleIdentity capsule) {
            this.capsule = Objects.requireNonNull(capsule, "capsule");
        }

        /** Adds an advanced input value represented directly by the core SDK. */
        public Builder input(String name, ProfileInputValue value) {
            inputs.put(Objects.requireNonNull(name, "name"), Objects.requireNonNull(value, "value"));
            return this;
        }

        public Builder artifact(String name, LocalInputArtifact value) {
            return input(name, value);
        }

        public Builder string(String name, String value) {
            return input(name, new StringProfileInput(value));
        }

        public Builder int64(String name, long value) {
            return input(name, new Int64ProfileInput(value));
        }

        public Builder bool(String name, boolean value) {
            return input(name, new BooleanProfileInput(value));
        }

        public Builder resourceOverrides(ProfileResourceOverrides value) {
            resourceOverrides = Objects.requireNonNull(value, "resourceOverrides");
            return this;
        }

        public CapsuleRequest build() {
            return new CapsuleRequest(capsule, inputs, resourceOverrides);
        }
    }
}
