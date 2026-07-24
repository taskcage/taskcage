package io.github.taskcage.sdk.internal.transport;

import java.io.EOFException;
import java.io.IOException;
import java.nio.ByteBuffer;
import java.nio.channels.SelectionKey;
import java.nio.channels.Selector;
import java.nio.channels.SocketChannel;
import java.time.Duration;
import java.util.concurrent.TimeUnit;

/** Protocol v1's four-byte, unsigned big-endian length-prefixed frame codec. */
public final class LengthPrefixedFrameCodec {
    public static final int MAX_FRAME_BYTES = 1_048_576;

    private LengthPrefixedFrameCodec() {
    }

    public static void write(SocketChannel channel, byte[] payload, Duration timeout) throws IOException {
        write(channel, payload, deadlineAfter(timeout));
    }

    static void write(SocketChannel channel, byte[] payload, long deadlineNanos) throws IOException {
        if (payload.length == 0 || payload.length > MAX_FRAME_BYTES) {
            throw new IOException("payload length must be between 1 and " + MAX_FRAME_BYTES);
        }

        ByteBuffer header = ByteBuffer.allocate(Integer.BYTES).putInt(payload.length);
        header.flip();
        writeFully(channel, header, deadlineNanos);
        writeFully(channel, ByteBuffer.wrap(payload), deadlineNanos);
    }

    public static byte[] read(SocketChannel channel, Duration timeout) throws IOException {
        return read(channel, deadlineAfter(timeout));
    }

    static byte[] read(SocketChannel channel, long deadlineNanos) throws IOException {
        ByteBuffer header = ByteBuffer.allocate(Integer.BYTES);
        readFully(channel, header, deadlineNanos);
        header.flip();

        long length = Integer.toUnsignedLong(header.getInt());
        if (length == 0 || length > MAX_FRAME_BYTES) {
            throw new IOException("invalid frame length: " + length);
        }

        ByteBuffer payload = ByteBuffer.allocate((int) length);
        readFully(channel, payload, deadlineNanos);
        return payload.array();
    }

    static long deadlineAfter(Duration timeout) {
        long timeoutNanos = timeout.toNanos();
        if (timeoutNanos <= 0) {
            throw new IllegalArgumentException("timeout must be positive");
        }
        return Math.addExact(System.nanoTime(), timeoutNanos);
    }

    private static void writeFully(SocketChannel channel, ByteBuffer buffer, long deadlineNanos) throws IOException {
        while (buffer.hasRemaining()) {
            if (channel.write(buffer) == 0) {
                await(channel, SelectionKey.OP_WRITE, deadlineNanos);
            }
        }
    }

    private static void readFully(SocketChannel channel, ByteBuffer buffer, long deadlineNanos) throws IOException {
        while (buffer.hasRemaining()) {
            int read = channel.read(buffer);
            if (read < 0) {
                throw new EOFException("socket closed before a complete frame was received");
            }
            if (read == 0) {
                await(channel, SelectionKey.OP_READ, deadlineNanos);
            }
        }
    }

    private static void await(SocketChannel channel, int operation, long deadlineNanos) throws IOException {
        if (channel.isBlocking()) {
            throw new IOException("blocking socket made no progress");
        }
        try (Selector selector = Selector.open()) {
            channel.register(selector, operation);
            long remainingNanos = deadlineNanos - System.nanoTime();
            if (remainingNanos <= 0) {
                throw new IOException("timed out waiting for a TaskCage daemon response");
            }
            long waitMillis = Math.max(1, TimeUnit.NANOSECONDS.toMillis(remainingNanos));
            if (selector.select(waitMillis) == 0) {
                throw new IOException("timed out waiting for a TaskCage daemon response");
            }
        }
    }
}
