package org.taskcage.sdk;

import java.time.Duration;
import java.util.Objects;
import java.util.Optional;

/** Optional per-request overrides of an installed Profile's resource defaults. */
public final class ProfileResourceOverrides {
    private static final ProfileResourceOverrides NONE = new Builder().build();

    private final CpuQuota cpuMax;
    private final Long memoryMaxBytes;
    private final Long pidsMax;
    private final Duration wallTimeLimit;
    private final Integer stdoutTailMaxBytes;
    private final Integer stderrTailMaxBytes;

    private ProfileResourceOverrides(Builder builder) {
        this.cpuMax = builder.cpuMax;
        this.memoryMaxBytes = builder.memoryMaxBytes;
        this.pidsMax = builder.pidsMax;
        this.wallTimeLimit = builder.wallTimeLimit;
        this.stdoutTailMaxBytes = builder.stdoutTailMaxBytes;
        this.stderrTailMaxBytes = builder.stderrTailMaxBytes;
    }

    /** Returns an override value that causes the wire field to be omitted. */
    public static ProfileResourceOverrides none() {
        return NONE;
    }

    public static Builder builder() {
        return new Builder();
    }

    public Optional<CpuQuota> cpuMax() {
        return Optional.ofNullable(cpuMax);
    }

    public Optional<Long> memoryMaxBytes() {
        return Optional.ofNullable(memoryMaxBytes);
    }

    public Optional<Long> pidsMax() {
        return Optional.ofNullable(pidsMax);
    }

    public Optional<Duration> wallTimeLimit() {
        return Optional.ofNullable(wallTimeLimit);
    }

    public Optional<Integer> stdoutTailMaxBytes() {
        return Optional.ofNullable(stdoutTailMaxBytes);
    }

    public Optional<Integer> stderrTailMaxBytes() {
        return Optional.ofNullable(stderrTailMaxBytes);
    }

    public boolean isEmpty() {
        return cpuMax == null
                && memoryMaxBytes == null
                && pidsMax == null
                && wallTimeLimit == null
                && stdoutTailMaxBytes == null
                && stderrTailMaxBytes == null;
    }

    /** Builds immutable overrides while preserving exactly which fields were supplied. */
    public static final class Builder {
        private CpuQuota cpuMax;
        private Long memoryMaxBytes;
        private Long pidsMax;
        private Duration wallTimeLimit;
        private Integer stdoutTailMaxBytes;
        private Integer stderrTailMaxBytes;

        private Builder() {}

        public Builder cpuMax(CpuQuota value) {
            this.cpuMax = Objects.requireNonNull(value, "cpuMax");
            return this;
        }

        public Builder memoryMaxBytes(long value) {
            requirePositive(value, "memoryMaxBytes");
            this.memoryMaxBytes = value;
            return this;
        }

        public Builder pidsMax(long value) {
            requirePositive(value, "pidsMax");
            this.pidsMax = value;
            return this;
        }

        public Builder wallTimeLimit(Duration value) {
            ResourceBudget.requirePositiveWholeMilliseconds(
                    Objects.requireNonNull(value, "wallTimeLimit"));
            this.wallTimeLimit = value;
            return this;
        }

        public Builder stdoutTailMaxBytes(int value) {
            requireTailLimit(value, "stdoutTailMaxBytes");
            this.stdoutTailMaxBytes = value;
            return this;
        }

        public Builder stderrTailMaxBytes(int value) {
            requireTailLimit(value, "stderrTailMaxBytes");
            this.stderrTailMaxBytes = value;
            return this;
        }

        public ProfileResourceOverrides build() {
            if (stdoutTailMaxBytes != null
                    && stderrTailMaxBytes != null
                    && stdoutTailMaxBytes + stderrTailMaxBytes > 131_072) {
                throw new IllegalArgumentException("combined output tail limits must not exceed 131072 bytes");
            }
            return new ProfileResourceOverrides(this);
        }

        private static void requirePositive(long value, String name) {
            if (value <= 0) {
                throw new IllegalArgumentException(name + " must be positive");
            }
        }

        private static void requireTailLimit(int value, String name) {
            if (value < 1 || value > 65_536) {
                throw new IllegalArgumentException(name + " must be between 1 and 65536 bytes");
            }
        }
    }
}
