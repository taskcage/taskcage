package org.taskcage.sdk.internal.remote;

import com.fasterxml.jackson.databind.JsonNode;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.SocketTimeoutException;
import java.nio.file.FileAlreadyExistsException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.nio.file.StandardOpenOption;
import java.security.MessageDigest;
import java.time.Duration;
import java.util.Objects;
import java.util.UUID;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.locks.ReentrantLock;
import org.taskcage.sdk.*;

/** Serialized, reconnect-on-next-call Remote Protocol v1 client. */
public final class DefaultRemoteTaskCageClient implements RemoteTaskCageClient {
    private static final int DOWNLOAD_TEMP_ATTEMPTS = 10;

    private final RemoteConnectionOptions options;
    private final RemoteFrameConnectionFactory connectionFactory;
    private final RemoteProtocolCodec codec = new RemoteProtocolCodec();
    private final ReentrantLock requestLock = new ReentrantLock();
    private RemoteFrameConnection connection;
    private RemoteCapabilities capabilities;

    public DefaultRemoteTaskCageClient(RemoteConnectionOptions options) {
        this(options, TlsFrameConnection::connect);
    }

    DefaultRemoteTaskCageClient(
            RemoteConnectionOptions options, RemoteFrameConnectionFactory connectionFactory) {
        this.options = Objects.requireNonNull(options, "options");
        this.connectionFactory = Objects.requireNonNull(connectionFactory, "connectionFactory");
    }

    @Override public RemoteCapabilities capabilities() {
        requestLock.lock();
        try {
            if (capabilities == null) {
                capabilities = codec.decodeCapabilities(call(codec.getCapabilities(UUID.randomUUID())));
            }
            return capabilities;
        } finally {
            requestLock.unlock();
        }
    }

    @Override public RemoteArtifactUpload upload(Path source, String mediaType) throws IOException { return upload(UUID.randomUUID(), source, mediaType); }

    @Override public RemoteArtifactUpload upload(UUID clientArtifactId, Path source, String mediaType) throws IOException {
        requestLock.lock();
        try {
            long sizeBytes = Files.size(source);
            if (sizeBytes <= 0) throw new IllegalArgumentException("source must be a non-empty file");
            Sha256Digest digest = sha256(source);
            RemoteArtifactUploadStart start = codec.decodeArtifactUploadStarted(call(codec.beginArtifactUpload(
                    UUID.randomUUID(), clientArtifactId, digest, sizeBytes, mediaType)));
            long offset = start.nextOffset();
            if (offset > sizeBytes) throw new TaskCageProtocolException("Remote Artifact resume offset exceeds source size");
            int chunkSize = Math.min(capabilities().maxArtifactChunkBytes(), 780000);
            try (InputStream input = Files.newInputStream(source)) {
                skipFully(input, offset);
                byte[] buffer = new byte[chunkSize];
                while (offset < sizeBytes) {
                    int count = input.read(buffer, 0, (int) Math.min(buffer.length, sizeBytes - offset));
                    if (count < 0) throw new IOException("source file changed while uploading");
                    byte[] chunk = java.util.Arrays.copyOf(buffer, count);
                    long expectedOffset = offset + count;
                    long acceptedOffset = codec.decodeArtifactChunkAccepted(call(
                            codec.uploadArtifactChunk(UUID.randomUUID(), start.artifactId(), offset, chunk))).nextOffset();
                    if (acceptedOffset != expectedOffset) {
                        throw new TaskCageProtocolException("Remote Artifact chunk acknowledgement has an unexpected offset");
                    }
                    offset = acceptedOffset;
                }
            }
            return codec.decodeArtifactUploaded(call(codec.completeArtifactUpload(UUID.randomUUID(), start.artifactId())));
        } finally {
            requestLock.unlock();
        }
    }

    @Override public void download(ManagedOutputArtifact artifact, Path destination) throws IOException {
        requestLock.lock();
        try {
            long offset = 0;
            MessageDigest digest = newSha256();
            try (DownloadTarget target = DownloadTarget.create(destination)) {
                OutputStream output = target.output();
                while (true) {
                    RemoteArtifactChunk chunk = codec.decodeArtifactChunk(call(codec.readArtifactChunk(UUID.randomUUID(), artifact.artifactId(), offset, capabilities().maxArtifactChunkBytes())));
                    if (chunk.offset() != offset) throw new TaskCageProtocolException("Remote Artifact chunk offset changed");
                    output.write(chunk.bytes());
                    digest.update(chunk.bytes());
                    offset = chunk.nextOffset();
                    if (offset > artifact.sizeBytes()) throw new TaskCageProtocolException("Remote Artifact is larger than its declared size");
                    if (chunk.finished()) break;
                }
                if (offset != artifact.sizeBytes() || !sha256(digest).equals(artifact.digest())) {
                    throw new TaskCageProtocolException("downloaded Artifact size or digest mismatch");
                }
                target.publish();
            }
        } finally {
            requestLock.unlock();
        }
    }

    @Override public RemoteProfileTask submitProfile(RemoteProfileRequest request) { return submitProfile(UUID.randomUUID(), request); }
    @Override public RemoteProfileTask submitProfile(UUID id, RemoteProfileRequest request) {
        requestLock.lock();
        try {
            return codec.decodeProfileAccepted(call(codec.submitProfile(UUID.randomUUID(), id, request)));
        } finally {
            requestLock.unlock();
        }
    }
    @Override public RemoteProfileTaskSnapshot getProfileResult(UUID taskId) {
        requestLock.lock();
        try {
            return codec.decodeProfileResult(call(codec.getProfileResult(UUID.randomUUID(), taskId)));
        } finally {
            requestLock.unlock();
        }
    }
    @Override public RemoteProfileTaskSnapshot getProfileResult(UUID taskId, Duration requestTimeout) {
        long timeoutNanos = requirePositiveNanos(requestTimeout, "requestTimeout");
        long startedAt = System.nanoTime();
        lockForRequest(timeoutNanos);
        try {
            return codec.decodeProfileResult(call(
                    codec.getProfileResult(UUID.randomUUID(), taskId),
                    remainingRequestDuration(startedAt, timeoutNanos)));
        } finally {
            requestLock.unlock();
        }
    }
    @Override public TaskCancellation cancelTask(UUID taskId) {
        requestLock.lock();
        try {
            return codec.decodeTaskCancelled(call(codec.cancelTask(UUID.randomUUID(), taskId)));
        } finally {
            requestLock.unlock();
        }
    }

    private JsonNode call(byte[] request) {
        return call(request, options.requestTimeout());
    }

    private JsonNode call(byte[] request, Duration requestTimeout) {
        long timeoutNanos = requirePositiveNanos(requestTimeout, "requestTimeout");
        long startedAt = System.nanoTime();
        UUID requestId;
        try { requestId = UUID.fromString(new com.fasterxml.jackson.databind.ObjectMapper().readTree(request).path("requestId").asText()); }
        catch (Exception e) { throw new TaskCageProtocolException("could not read Remote request id", e); }
        try {
            ensureConnection(shorter(
                    options.requestTimeout(), remainingDuration(startedAt, timeoutNanos)));
            connection.write(request);
            JsonNode response = codec.readAndValidate(
                    connection.read(shorter(
                            options.requestTimeout(), remainingDuration(startedAt, timeoutNanos))),
                    requestId);
            if ("error".equals(response.path("type").asText())) throw codec.decodeError(response);
            return response;
        } catch (IOException e) { closeConnection(); throw new TaskCageConnectionException("Remote TaskCage TLS connection failed", e); }
    }

    private void ensureConnection(Duration requestTimeout) throws IOException {
        if (connection != null) return;
        long timeoutNanos = requirePositiveNanos(requestTimeout, "requestTimeout");
        long startedAt = System.nanoTime();
        try {
            connection = connectionFactory.connect(
                    options, remainingDuration(startedAt, timeoutNanos));
            UUID id = UUID.randomUUID(); connection.write(codec.authenticate(id, options.credentials()));
            JsonNode response = codec.readAndValidate(
                    connection.read(remainingDuration(startedAt, timeoutNanos)), id);
            if ("error".equals(response.path("type").asText())) throw codec.decodeError(response);
            codec.requireAuthenticated(response);
        } catch (IOException | RuntimeException exception) {
            closeConnection();
            throw exception;
        }
    }
    private void closeConnection() { if (connection != null) try { connection.close(); } catch (IOException ignored) {} finally { connection = null; } }
    @Override public void close() {
        requestLock.lock();
        try {
            closeConnection();
        } finally {
            requestLock.unlock();
        }
    }
    private static void skipFully(InputStream input, long offset) throws IOException {
        long remaining = offset;
        while (remaining > 0) {
            long skipped = input.skip(remaining);
            if (skipped > 0) {
                remaining -= skipped;
            } else if (input.read() == -1) {
                throw new IOException("source file changed while resuming upload");
            } else {
                remaining--;
            }
        }
    }

    static Sha256Digest sha256(Path source) throws IOException {
        MessageDigest digest = newSha256();
        try (InputStream input = Files.newInputStream(source)) {
            byte[] buffer = new byte[32 * 1024];
            for (int count; (count = input.read(buffer)) >= 0;) digest.update(buffer, 0, count);
        }
        return sha256(digest);
    }

    private static MessageDigest newSha256() {
        try { return MessageDigest.getInstance("SHA-256"); }
        catch (Exception e) { throw new IllegalStateException("SHA-256 unavailable", e); }
    }

    private static Sha256Digest sha256(MessageDigest digest) {
        return new Sha256Digest("sha256:" + java.util.HexFormat.of().formatHex(digest.digest()));
    }

    private static long requirePositiveNanos(Duration duration, String name) {
        Objects.requireNonNull(duration, name);
        try {
            long nanos = duration.toNanos();
            if (nanos <= 0) {
                throw new IllegalArgumentException(
                        name + " must be positive and representable in nanoseconds");
            }
            return nanos;
        } catch (ArithmeticException exception) {
            throw new IllegalArgumentException(
                    name + " must be representable in nanoseconds", exception);
        }
    }

    private static Duration remainingDuration(long startedAt, long timeoutNanos)
            throws SocketTimeoutException {
        long remainingNanos = timeoutNanos - (System.nanoTime() - startedAt);
        if (remainingNanos <= 0) {
            throw new SocketTimeoutException("Remote request timeout");
        }
        return Duration.ofNanos(remainingNanos);
    }

    private void lockForRequest(long timeoutNanos) {
        try {
            if (!requestLock.tryLock(timeoutNanos, TimeUnit.NANOSECONDS)) {
                throw requestTimeout("timed out waiting to send a Remote TaskCage request", null);
            }
        } catch (InterruptedException exception) {
            Thread.currentThread().interrupt();
            throw new TaskCageConnectionException(
                    "interrupted while waiting to send a Remote TaskCage request", exception);
        }
    }

    private static Duration remainingRequestDuration(long startedAt, long timeoutNanos) {
        try {
            return remainingDuration(startedAt, timeoutNanos);
        } catch (SocketTimeoutException exception) {
            throw requestTimeout("Remote TaskCage request timed out", exception);
        }
    }

    private static Duration shorter(Duration first, Duration second) {
        return first.compareTo(second) <= 0 ? first : second;
    }

    private static TaskCageConnectionException requestTimeout(String message, Throwable cause) {
        SocketTimeoutException timeout = new SocketTimeoutException(message);
        if (cause != null) {
            timeout.initCause(cause);
        }
        return new TaskCageConnectionException(message, timeout);
    }

    private static final class DownloadTarget implements AutoCloseable {
        private final Path destination;
        private final Path temporary;
        private OutputStream output;
        private boolean published;

        private DownloadTarget(Path destination, Path temporary, OutputStream output) {
            this.destination = destination;
            this.temporary = temporary;
            this.output = output;
        }

        static DownloadTarget create(Path destination) throws IOException {
            Objects.requireNonNull(destination, "destination");
            Path fileName = Objects.requireNonNull(
                    destination.getFileName(), "destination must name a file");
            for (int attempt = 0; attempt < DOWNLOAD_TEMP_ATTEMPTS; attempt++) {
                Path temporary = destination.resolveSibling(
                        "." + fileName + ".taskcage-" + UUID.randomUUID() + ".part");
                try {
                    OutputStream output = Files.newOutputStream(
                            temporary, StandardOpenOption.CREATE_NEW, StandardOpenOption.WRITE);
                    return new DownloadTarget(destination, temporary, output);
                } catch (FileAlreadyExistsException collision) {
                    // Try another exclusive UUID path without opening the existing entry.
                }
            }
            throw new IOException("could not create a unique Remote Artifact download file");
        }

        OutputStream output() {
            if (output == null) {
                throw new IllegalStateException("download output is already closed");
            }
            return output;
        }

        void publish() throws IOException {
            closeOutput();
            Files.move(
                    temporary,
                    destination,
                    StandardCopyOption.REPLACE_EXISTING,
                    StandardCopyOption.ATOMIC_MOVE);
            published = true;
        }

        @Override
        public void close() throws IOException {
            IOException failure = null;
            try {
                closeOutput();
            } catch (IOException exception) {
                failure = exception;
            }
            if (!published) {
                try {
                    Files.deleteIfExists(temporary);
                } catch (IOException exception) {
                    if (failure == null) {
                        failure = exception;
                    } else {
                        failure.addSuppressed(exception);
                    }
                }
            }
            if (failure != null) {
                throw failure;
            }
        }

        private void closeOutput() throws IOException {
            if (output == null) {
                return;
            }
            OutputStream active = output;
            output = null;
            active.close();
        }
    }
}
