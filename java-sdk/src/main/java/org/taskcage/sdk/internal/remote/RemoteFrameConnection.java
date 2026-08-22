package org.taskcage.sdk.internal.remote;

import java.io.IOException;
import java.time.Duration;
import org.taskcage.sdk.RemoteConnectionOptions;

/** Timeout-aware frame connection used by the Remote Protocol client. */
interface RemoteFrameConnection extends AutoCloseable {
    void write(byte[] payload) throws IOException;

    byte[] read(Duration timeout) throws IOException;

    @Override
    void close() throws IOException;
}

@FunctionalInterface
interface RemoteFrameConnectionFactory {
    RemoteFrameConnection connect(RemoteConnectionOptions options, Duration timeout) throws IOException;
}
