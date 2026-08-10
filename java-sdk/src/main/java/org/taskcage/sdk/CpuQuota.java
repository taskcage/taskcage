package org.taskcage.sdk;

/** cgroup v2 {@code cpu.max} quota and period values in microseconds. */
public record CpuQuota(long quotaMicros, long periodMicros) {
    public CpuQuota {
        if (quotaMicros <= 0 || periodMicros <= 0) {
            throw new IllegalArgumentException("cpu quota and period must be positive");
        }
    }
}
