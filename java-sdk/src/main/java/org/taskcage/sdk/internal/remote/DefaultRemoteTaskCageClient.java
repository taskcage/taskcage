package org.taskcage.sdk.internal.remote;

import com.fasterxml.jackson.databind.JsonNode;
import java.io.IOException;
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
        byte[] bytes = Files.readAllBytes(source);
        Sha256Digest digest = sha256(bytes);
        RemoteArtifactUploadStart start = codec.decodeArtifactUploadStarted(call(codec.beginArtifactUpload(UUID.randomUUID(), clientArtifactId, digest, bytes.length, mediaType)));
        long offset = start.nextOffset();
        int chunkSize = Math.min(capabilities().maxArtifactChunkBytes(), 780000);
        while (offset < bytes.length) {
            int count = Math.min(chunkSize, bytes.length - (int) offset);
            byte[] chunk = java.util.Arrays.copyOfRange(bytes, (int) offset, (int) offset + count);
            offset = codec.decodeArtifactChunkAccepted(call(codec.uploadArtifactChunk(UUID.randomUUID(), start.artifactId(), offset, chunk))).nextOffset();
        }
        return codec.decodeArtifactUploaded(call(codec.completeArtifactUpload(UUID.randomUUID(), start.artifactId())));
    }

    @Override public synchronized void download(ManagedOutputArtifact artifact, Path destination) throws IOException {
        Path temp = destination.resolveSibling(destination.getFileName() + ".taskcage-part");
        long offset = 0;
        try (var output = Files.newOutputStream(temp)) {
            while (true) {
                RemoteArtifactChunk chunk = codec.decodeArtifactChunk(call(codec.readArtifactChunk(UUID.randomUUID(), artifact.artifactId(), offset, capabilities().maxArtifactChunkBytes())));
                if (chunk.offset() != offset) throw new TaskCageProtocolException("Remote Artifact chunk offset changed");
                output.write(chunk.bytes()); offset = chunk.nextOffset(); if (chunk.finished()) break;
            }
        }
        if (!sha256(Files.readAllBytes(temp)).equals(artifact.digest())) { Files.deleteIfExists(temp); throw new TaskCageProtocolException("downloaded Artifact digest mismatch"); }
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
        connection = TlsFrameConnection.connect(options);
        UUID id = UUID.randomUUID(); connection.write(codec.authenticate(id, options.credentials()));
        codec.requireAuthenticated(codec.readAndValidate(connection.read(), id));
    }
    private void closeConnection() { if (connection != null) try { connection.close(); } catch (IOException ignored) {} finally { connection = null; } }
    @Override public synchronized void close() { closeConnection(); }
    private static Sha256Digest sha256(byte[] bytes) {
        try { return new Sha256Digest("sha256:" + java.util.HexFormat.of().formatHex(MessageDigest.getInstance("SHA-256").digest(bytes))); }
        catch (Exception e) { throw new IllegalStateException("SHA-256 unavailable", e); }
    }
}
