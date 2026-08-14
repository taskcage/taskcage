package org.taskcage.sdk.internal.remote;

import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.io.EOFException;
import java.io.IOException;
import java.net.InetSocketAddress;
import java.net.Socket;
import java.time.Duration;
import javax.net.ssl.SSLParameters;
import javax.net.ssl.SSLSocket;
import org.taskcage.sdk.RemoteConnectionOptions;

/** Blocking TLS 1.3 connection for the Remote Protocol's length-prefixed frames. */
final class TlsFrameConnection implements AutoCloseable {
    private static final int MAX_FRAME_BYTES = 1_048_576;
    private final SSLSocket socket;
    private final DataInputStream input;
    private final DataOutputStream output;

    static TlsFrameConnection connect(RemoteConnectionOptions options) throws IOException {
        Socket plain = new Socket();
        plain.connect(new InetSocketAddress(options.endpoint().getHost(), options.endpoint().getPort()), timeoutMillis(options.connectTimeout()));
        SSLSocket socket = (SSLSocket) options.sslContext().getSocketFactory().createSocket(
                plain, options.endpoint().getHost(), options.endpoint().getPort(), true);
        SSLParameters parameters = socket.getSSLParameters();
        parameters.setProtocols(new String[] {"TLSv1.3"});
        parameters.setEndpointIdentificationAlgorithm("HTTPS");
        parameters.setApplicationProtocols(new String[] {"taskcage/1"});
        socket.setSSLParameters(parameters);
        socket.setSoTimeout(timeoutMillis(options.requestTimeout()));
        socket.startHandshake();
        if (!"taskcage/1".equals(socket.getApplicationProtocol())) {
            socket.close();
            throw new IOException("TaskCage daemon did not negotiate ALPN taskcage/1");
        }
        return new TlsFrameConnection(socket);
    }

    private TlsFrameConnection(SSLSocket socket) throws IOException {
        this.socket = socket;
        input = new DataInputStream(socket.getInputStream());
        output = new DataOutputStream(socket.getOutputStream());
    }

    void write(byte[] payload) throws IOException {
        if (payload.length == 0 || payload.length > MAX_FRAME_BYTES) throw new IOException("invalid Remote frame length");
        output.writeInt(payload.length);
        output.write(payload);
        output.flush();
    }

    byte[] read() throws IOException {
        long length = Integer.toUnsignedLong(input.readInt());
        if (length == 0 || length > MAX_FRAME_BYTES) throw new IOException("invalid Remote frame length: " + length);
        byte[] payload = input.readNBytes((int) length);
        if (payload.length != length) throw new EOFException("Remote daemon closed before a complete frame");
        return payload;
    }

    private static int timeoutMillis(Duration duration) {
        long millis = Math.max(1, (duration.toNanos() + 999_999L) / 1_000_000L);
        return Math.toIntExact(Math.min(Integer.MAX_VALUE, millis));
    }

    @Override public void close() throws IOException { socket.close(); }
}
