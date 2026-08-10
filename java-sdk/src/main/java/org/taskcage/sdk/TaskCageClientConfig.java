package org.taskcage.sdk;

import java.nio.file.Path;
import java.time.Duration;
import java.util.Objects;

/** Immutable transport configuration for {@link TaskCageClient}. */
public record TaskCageClientConfig(Path socketPath, Duration connectTimeout, Duration requestTimeout) {
    public TaskCageClientConfig {
        socketPath = Objects.requireNonNull(socketPath, "socketPath").toAbsolutePath();
        connectTimeout = requirePositive(connectTimeout, "connectTimeout");
        requestTimeout = requirePositive(requestTimeout, "requestTimeout");
    }

    public static Builder builder() {
        return new Builder();
    }

    private static Duration requirePositive(Duration value, String name) {
        Objects.requireNonNull(value, name);
        if (value.isZero() || value.isNegative()) {
            throw new IllegalArgumentException(name + " must be positive");
        }
        return value;
    }

    public static final class Builder {
        private Path socketPath;
        private Duration connectTimeout = Duration.ofSeconds(1);
        private Duration requestTimeout = Duration.ofSeconds(5);

        public Builder socketPath(Path socketPath) {
            this.socketPath = socketPath;
            return this;
        }

        public Builder connectTimeout(Duration connectTimeout) {
            this.connectTimeout = connectTimeout;
            return this;
        }

        public Builder requestTimeout(Duration requestTimeout) {
            this.requestTimeout = requestTimeout;
            return this;
        }

        public TaskCageClientConfig build() {
            return new TaskCageClientConfig(socketPath, connectTimeout, requestTimeout);
        }
    }
}
