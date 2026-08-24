package org.taskcage.sdk.internal.remote;

import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.io.EOFException;
import java.io.IOException;
import java.net.InetSocketAddress;
import java.net.Socket;
import java.net.SocketTimeoutException;
import java.nio.ByteBuffer;
import java.time.Duration;
import javax.net.ssl.SSLParameters;
import javax.net.ssl.SSLSocket;
import org.taskcage.sdk.RemoteConnectionOptions;
import org.taskcage.sdk.TlsVerificationMode;

/** Blocking TLS 1.3 connection for the Remote Protocol's length-prefixed frames. */
final class TlsFrameConnection implements RemoteFrameConnection {
    private static final int MAX_FRAME_BYTES = 1_048_576;
    private final SSLSocket socket;
    private final DataInputStream input;
    private final DataOutputStream output;

    static TlsFrameConnection connect(RemoteConnectionOptions options, Duration timeout) throws IOException {
        long timeoutNanos = requirePositiveNanos(timeout);
        long startedAt = System.nanoTime();
        Socket plain = new Socket();
        Socket active = plain;
        try {
            Duration connectTimeout = shorter(
                    options.connectTimeout(), remainingDuration(startedAt, timeoutNanos));
            plain.connect(
                    new InetSocketAddress(options.endpoint().getHost(), options.endpoint().getPort()),
                    timeoutMillis(connectTimeout));
            SSLSocket socket = (SSLSocket) options.sslContext().getSocketFactory().createSocket(
                    plain, options.endpoint().getHost(), options.endpoint().getPort(), true);
            active = socket;
            SSLParameters parameters = socket.getSSLParameters();
            parameters.setProtocols(new String[] {"TLSv1.3"});
            if (options.tlsVerification() == TlsVerificationMode.VERIFY_IDENTITY) {
                parameters.setEndpointIdentificationAlgorithm("HTTPS");
            }
            parameters.setApplicationProtocols(new String[] {"taskcage/remote/1"});
            socket.setSSLParameters(parameters);
            socket.setSoTimeout(timeoutMillis(remainingDuration(startedAt, timeoutNanos)));
            socket.startHandshake();
            if (!"taskcage/remote/1".equals(socket.getApplicationProtocol())) {
                throw new IOException("TaskCage daemon did not negotiate ALPN taskcage/remote/1");
            }
            return new TlsFrameConnection(socket);
        } catch (IOException | RuntimeException exception) {
            try {
                active.close();
            } catch (IOException closeException) {
                exception.addSuppressed(closeException);
            }
            throw exception;
        }
    }

    private TlsFrameConnection(SSLSocket socket) throws IOException {
        this.socket = socket;
        input = new DataInputStream(socket.getInputStream());
        output = new DataOutputStream(socket.getOutputStream());
    }

    @Override
    public void write(byte[] payload) throws IOException {
        if (payload.length == 0 || payload.length > MAX_FRAME_BYTES) throw new IOException("invalid Remote frame length");
        output.writeInt(payload.length);
        output.write(payload);
        output.flush();
    }

    @Override
    public byte[] read(Duration timeout) throws IOException {
        long timeoutNanos = requirePositiveNanos(timeout);
        long startedAt = System.nanoTime();
        byte[] header = readFully(4, startedAt, timeoutNanos);
        long length = Integer.toUnsignedLong(ByteBuffer.wrap(header).getInt());
        if (length == 0 || length > MAX_FRAME_BYTES) throw new IOException("invalid Remote frame length: " + length);
        return readFully((int) length, startedAt, timeoutNanos);
    }

    private byte[] readFully(int length, long startedAt, long timeoutNanos) throws IOException {
        byte[] bytes = new byte[length];
        int offset = 0;
        while (offset < length) {
            socket.setSoTimeout(timeoutMillis(remainingDuration(startedAt, timeoutNanos)));
            int count = input.read(bytes, offset, length - offset);
            if (count < 0) {
                throw new EOFException("Remote daemon closed before a complete frame");
            }
            offset += count;
        }
        return bytes;
    }

    static int timeoutMillis(Duration duration) {
        long nanos = requirePositiveNanos(duration);
        long millis = nanos / 1_000_000L;
        if (nanos % 1_000_000L != 0) {
            millis++;
        }
        return Math.toIntExact(Math.min(Integer.MAX_VALUE, millis));
    }

    private static long requirePositiveNanos(Duration duration) {
        long nanos;
        try {
            nanos = duration.toNanos();
        } catch (ArithmeticException exception) {
            throw new IllegalArgumentException("timeout must be representable in nanoseconds", exception);
        }
        if (nanos <= 0) {
            throw new IllegalArgumentException("timeout must be positive and representable in nanoseconds");
        }
        return nanos;
    }

    private static Duration remainingDuration(long startedAt, long timeoutNanos)
            throws SocketTimeoutException {
        long remainingNanos = timeoutNanos - (System.nanoTime() - startedAt);
        if (remainingNanos <= 0) {
            throw new SocketTimeoutException("Remote TLS connection timeout");
        }
        return Duration.ofNanos(remainingNanos);
    }

    private static Duration shorter(Duration first, Duration second) {
        return first.compareTo(second) <= 0 ? first : second;
    }

    @Override
    public void close() throws IOException {
        socket.close();
    }
}
