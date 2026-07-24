package io.github.taskcage.sdk;

import java.util.List;
import java.util.Objects;

/** Capabilities reported by a compatible TaskCage daemon. */
public record TaskCageCapabilities(
        String daemonVersion,
        List<Integer> protocolVersions,
        int maxFrameBytes,
        int maxConcurrentTasks,
        boolean cgroupV2Ready) {
    public TaskCageCapabilities {
        daemonVersion = Objects.requireNonNull(daemonVersion, "daemonVersion");
        protocolVersions = List.copyOf(Objects.requireNonNull(protocolVersions, "protocolVersions"));
        if (maxFrameBytes <= 0 || maxConcurrentTasks <= 0) {
            throw new IllegalArgumentException("daemon capability limits must be positive");
        }
    }
}
