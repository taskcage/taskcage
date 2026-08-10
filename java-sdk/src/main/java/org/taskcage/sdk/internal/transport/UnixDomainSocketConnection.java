package org.taskcage.sdk.internal.transport;

import java.io.IOException;
import java.net.StandardProtocolFamily;
import java.net.UnixDomainSocketAddress;
import java.nio.channels.SelectionKey;
import java.nio.channels.Selector;
import java.nio.channels.SocketChannel;
import java.nio.file.Path;
import java.time.Duration;

/** A single request-response connection to a Unix domain socket. */
public final class UnixDomainSocketConnection implements AutoCloseable {
    private final SocketChannel channel;

    private UnixDomainSocketConnection(SocketChannel channel) {
        this.channel = channel;
    }

    public static UnixDomainSocketConnection connect(Path socketPath, Duration timeout) throws IOException {
        SocketChannel channel = SocketChannel.open(StandardProtocolFamily.UNIX);
        try {
            channel.configureBlocking(false);
            boolean connected = channel.connect(UnixDomainSocketAddress.of(socketPath));
            if (!connected) {
                finishConnect(channel, timeout);
            }
            return new UnixDomainSocketConnection(channel);
        } catch (IOException | RuntimeException exception) {
            channel.close();
            throw exception;
        }
    }

    public byte[] request(byte[] payload, Duration timeout) throws IOException {
        long deadlineNanos = LengthPrefixedFrameCodec.deadlineAfter(timeout);
        LengthPrefixedFrameCodec.write(channel, payload, deadlineNanos);
        return LengthPrefixedFrameCodec.read(channel, deadlineNanos);
    }

    @Override
    public void close() throws IOException {
        channel.close();
    }

    private static void finishConnect(SocketChannel channel, Duration timeout) throws IOException {
        long deadlineNanos = LengthPrefixedFrameCodec.deadlineAfter(timeout);
        try (Selector selector = Selector.open()) {
            channel.register(selector, SelectionKey.OP_CONNECT);
            while (!channel.finishConnect()) {
                long remainingNanos = deadlineNanos - System.nanoTime();
                if (remainingNanos <= 0) {
                    throw new IOException("timed out connecting to TaskCage daemon");
                }
                if (selector.select(LengthPrefixedFrameCodec.timeoutMillisCeiling(remainingNanos)) == 0
                        && Thread.currentThread().isInterrupted()) {
                    throw new IOException("interrupted while connecting to TaskCage daemon");
                }
            }
        }
    }
}
