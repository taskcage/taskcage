package org.taskcage.sdk.internal.client;

import java.io.IOException;
import java.time.Duration;
import org.taskcage.sdk.TaskCageClientConfig;
import org.taskcage.sdk.internal.transport.UnixDomainSocketConnection;

/** Owns the lazily-created Local Protocol connection and its client lifecycle. */
final class LocalConnectionManager {
    private final TaskCageClientConfig config;
    private UnixDomainSocketConnection connection;
    private boolean closed;

    LocalConnectionManager(TaskCageClientConfig config) {
        this.config = config;
    }

    boolean isClosed() {
        return closed;
    }

    Duration requestTimeout() {
        return config.requestTimeout();
    }

    UnixDomainSocketConnection requireConnection() throws IOException {
        if (connection == null) {
            connection = UnixDomainSocketConnection.connect(config.socketPath(), config.connectTimeout());
        }
        return connection;
    }

    UnixDomainSocketConnection requireConnectionWithin(Duration timeout) throws IOException {
        if (connection == null) {
            connection = UnixDomainSocketConnection.connect(
                    config.socketPath(), shorter(config.connectTimeout(), timeout));
        }
        return connection;
    }

    void close() {
        if (closed) {
            return;
        }
        closed = true;
        closeConnection();
    }

    void closeConnection() {
        if (connection == null) {
            return;
        }
        try {
            connection.close();
        } catch (IOException ignored) {
            // The channel is being discarded after a failure or explicit close.
        } finally {
            connection = null;
        }
    }

    private static Duration shorter(Duration first, Duration second) {
        return first.compareTo(second) <= 0 ? first : second;
    }
}
