package org.taskcage.sdk;

import java.nio.file.Path;
import java.util.Collections;
import java.util.Map;
import java.util.Objects;
import java.util.TreeMap;

/**
 * A Remote Capsule request that transfers one caller-owned input file and downloads one published output file.
 *
 * <p>The local {@link Path} values are SDK concerns: they are never sent to the daemon as Profile input values.
 * {@link RemoteCapsuleRunner} uploads the input over its authenticated TLS connection, submits the daemon-issued
 * Artifact reference, and downloads the requested output after successful execution.
 */
public final class RemoteCapsuleFileRequest {
    private final CapsuleIdentity capsule;
    private final String inputSlot;
    private final Path inputFile;
    private final String inputMediaType;
    private final Map<String, RemoteProfileInputValue> inputs;
    private final String outputSlot;
    private final Path outputFile;
    private final ProfileResourceOverrides resourceOverrides;

    private RemoteCapsuleFileRequest(Builder builder) {
        this.capsule = new CapsuleIdentity(builder.name, builder.version);
        this.inputSlot = requireSlot(builder.inputSlot, "inputSlot");
        this.inputFile = Objects.requireNonNull(builder.inputFile, "inputFile").toAbsolutePath().normalize();
        this.inputMediaType = requireNonBlank(builder.inputMediaType, "inputMediaType");
        this.outputSlot = requireSlot(builder.outputSlot, "outputSlot");
        this.outputFile = Objects.requireNonNull(builder.outputFile, "outputFile").toAbsolutePath().normalize();
        this.resourceOverrides = Objects.requireNonNull(builder.resourceOverrides, "resourceOverrides");

        TreeMap<String, RemoteProfileInputValue> copy = new TreeMap<>();
        builder.inputs.forEach((slot, value) -> {
            String validatedSlot = requireSlot(slot, "input slot");
            if (validatedSlot.equals(inputSlot)) {
                throw new IllegalArgumentException("input slot " + inputSlot + " is reserved for inputFile");
            }
            copy.put(validatedSlot, Objects.requireNonNull(value, "input value"));
        });
        this.inputs = Collections.unmodifiableMap(copy);
    }

    public static Builder builder(String name, String version) {
        return new Builder(name, version);
    }

    public CapsuleIdentity capsule() {
        return capsule;
    }

    public String inputSlot() {
        return inputSlot;
    }

    public Path inputFile() {
        return inputFile;
    }

    public String inputMediaType() {
        return inputMediaType;
    }

    public Map<String, RemoteProfileInputValue> inputs() {
        return inputs;
    }

    public String outputSlot() {
        return outputSlot;
    }

    public Path outputFile() {
        return outputFile;
    }

    public ProfileResourceOverrides resourceOverrides() {
        return resourceOverrides;
    }

    private static String requireSlot(String value, String name) {
        String slot = requireNonBlank(value, name);
        if (!slot.matches("[a-z][a-z0-9_-]{0,63}")) {
            throw new IllegalArgumentException(name + " must match [a-z][a-z0-9_-]{0,63}");
        }
        return slot;
    }

    private static String requireNonBlank(String value, String name) {
        Objects.requireNonNull(value, name);
        if (value.isBlank()) {
            throw new IllegalArgumentException(name + " must not be blank");
        }
        return value;
    }

    /** Builder for one local input file, scalar Profile inputs, and one local output destination. */
    public static final class Builder {
        private final String name;
        private final String version;
        private String inputSlot;
        private Path inputFile;
        private String inputMediaType;
        private final Map<String, RemoteProfileInputValue> inputs = new TreeMap<>();
        private String outputSlot;
        private Path outputFile;
        private ProfileResourceOverrides resourceOverrides = ProfileResourceOverrides.none();

        private Builder(String name, String version) {
            this.name = requireNonBlank(name, "name");
            this.version = requireNonBlank(version, "version");
        }

        public Builder inputFile(String slot, Path file, String mediaType) {
            this.inputSlot = requireSlot(slot, "input slot");
            this.inputFile = Objects.requireNonNull(file, "file");
            this.inputMediaType = requireNonBlank(mediaType, "mediaType");
            return this;
        }

        public Builder input(String slot, RemoteProfileInputValue value) {
            String inputSlot = requireSlot(slot, "input slot");
            if (inputs.putIfAbsent(inputSlot, Objects.requireNonNull(value, "value")) != null) {
                throw new IllegalArgumentException("input slot " + inputSlot + " was already supplied");
            }
            return this;
        }

        public Builder int64(String slot, long value) {
            return input(slot, new RemoteInt64Input(value));
        }

        public Builder outputFile(String slot, Path file) {
            this.outputSlot = requireSlot(slot, "output slot");
            this.outputFile = Objects.requireNonNull(file, "file");
            return this;
        }

        public Builder resourceOverrides(ProfileResourceOverrides value) {
            this.resourceOverrides = Objects.requireNonNull(value, "resourceOverrides");
            return this;
        }

        public RemoteCapsuleFileRequest build() {
            return new RemoteCapsuleFileRequest(this);
        }
    }
}
