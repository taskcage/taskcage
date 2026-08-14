package org.taskcage.sdk.internal.remote;

import com.fasterxml.jackson.databind.JsonNode;
import java.io.InputStream;
import java.io.IOException;
import java.io.OutputStream;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.MessageDigest;
import java.util.UUID;
import org.taskcage.sdk.*;

/** Serialized, reconnect-on-next-call Remote Protocol v1 client. */
public final class DefaultRemoteTaskCageClient implements RemoteTaskCageClient {
    private final RemoteConnectionOptions options;
    private final RemoteProtocolCodec codec = new RemoteProtocolCodec();
    private TlsFrameConnection connection;
    private RemoteCapabilities capabilities;

    public DefaultRemoteTaskCageClient(RemoteConnectionOptions options) { this.options = java.util.Objects.requireNonNull(options); }

    @Override public synchronized RemoteCapabilities capabilities() {
        if (capabilities == null) capabilities = codec.decodeCapabilities(call(codec.getCapabilities(UUID.randomUUID())));
        return capabilities;
    }

    @Override public RemoteArtifactUpload upload(Path source, String mediaType) throws IOException { return upload(UUID.randomUUID(), source, mediaType); }

    @Override public synchronized RemoteArtifactUpload upload(UUID clientArtifactId, Path source, String mediaType) throws IOException {
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
    }

    @Override public synchronized void download(ManagedOutputArtifact artifact, Path destination) throws IOException {
        Path temp = destination.resolveSibling(destination.getFileName() + ".taskcage-part");
        long offset = 0;
        MessageDigest digest = newSha256();
        try (OutputStream output = Files.newOutputStream(temp)) {
            while (true) {
                RemoteArtifactChunk chunk = codec.decodeArtifactChunk(call(codec.readArtifactChunk(UUID.randomUUID(), artifact.artifactId(), offset, capabilities().maxArtifactChunkBytes())));
                if (chunk.offset() != offset) throw new TaskCageProtocolException("Remote Artifact chunk offset changed");
                output.write(chunk.bytes());
                digest.update(chunk.bytes());
                offset = chunk.nextOffset();
                if (offset > artifact.sizeBytes()) throw new TaskCageProtocolException("Remote Artifact is larger than its declared size");
                if (chunk.finished()) break;
            }
        }
        if (offset != artifact.sizeBytes() || !sha256(digest).equals(artifact.digest())) {
            Files.deleteIfExists(temp);
            throw new TaskCageProtocolException("downloaded Artifact size or digest mismatch");
        }
        Files.move(temp, destination, java.nio.file.StandardCopyOption.REPLACE_EXISTING, java.nio.file.StandardCopyOption.ATOMIC_MOVE);
    }

    @Override public RemoteProfileTask submitProfile(RemoteProfileRequest request) { return submitProfile(UUID.randomUUID(), request); }
    @Override public synchronized RemoteProfileTask submitProfile(UUID id, RemoteProfileRequest request) { return codec.decodeProfileAccepted(call(codec.submitProfile(UUID.randomUUID(), id, request))); }
    @Override public synchronized RemoteProfileTaskSnapshot getProfileResult(UUID taskId) { return codec.decodeProfileResult(call(codec.getProfileResult(UUID.randomUUID(), taskId))); }
    @Override public synchronized TaskCancellation cancelTask(UUID taskId) { return codec.decodeTaskCancelled(call(codec.cancelTask(UUID.randomUUID(), taskId))); }

    private JsonNode call(byte[] request) {
        UUID requestId;
        try { requestId = UUID.fromString(new com.fasterxml.jackson.databind.ObjectMapper().readTree(request).path("requestId").asText()); }
        catch (Exception e) { throw new TaskCageProtocolException("could not read Remote request id", e); }
        try {
            ensureConnection(); connection.write(request); JsonNode response = codec.readAndValidate(connection.read(), requestId);
            if ("error".equals(response.path("type").asText())) throw codec.decodeError(response);
            return response;
        } catch (IOException e) { closeConnection(); throw new TaskCageConnectionException("Remote TaskCage TLS connection failed", e); }
    }

    private void ensureConnection() throws IOException {
        if (connection != null) return;
        try {
            connection = TlsFrameConnection.connect(options);
            UUID id = UUID.randomUUID(); connection.write(codec.authenticate(id, options.credentials()));
            JsonNode response = codec.readAndValidate(connection.read(), id);
            if ("error".equals(response.path("type").asText())) throw codec.decodeError(response);
            codec.requireAuthenticated(response);
        } catch (IOException | RuntimeException exception) {
            closeConnection();
            throw exception;
        }
    }
    private void closeConnection() { if (connection != null) try { connection.close(); } catch (IOException ignored) {} finally { connection = null; } }
    @Override public synchronized void close() { closeConnection(); }
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
}
