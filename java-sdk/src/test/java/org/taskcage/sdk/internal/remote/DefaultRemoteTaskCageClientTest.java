package org.taskcage.sdk.internal.remote;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;
import java.io.IOException;
import java.net.SocketTimeoutException;
import java.net.URI;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.MessageDigest;
import java.time.Duration;
import java.time.Instant;
import java.util.ArrayList;
import java.util.Base64;
import java.util.HexFormat;
import java.util.List;
import java.util.UUID;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicReference;
import org.junit.jupiter.api.Assumptions;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;
import org.taskcage.sdk.ManagedOutputArtifact;
import org.taskcage.sdk.RemoteConnectionOptions;
import org.taskcage.sdk.ServiceCredentials;
import org.taskcage.sdk.Sha256Digest;
import org.taskcage.sdk.Secret;
import org.taskcage.sdk.TaskCageConnectionException;

final class DefaultRemoteTaskCageClientTest {
    private static final ObjectMapper MAPPER = new ObjectMapper();
    private static final UUID ARTIFACT_ID =
            UUID.fromString("88888888-8888-4888-8888-888888888888");
    private static final UUID TASK_ID =
            UUID.fromString("55555555-5555-4555-8555-555555555555");
    private static final byte[] CONTENT = "test".getBytes(java.nio.charset.StandardCharsets.UTF_8);

    @TempDir
    Path directory;

    @Test
    void timeoutAwareResultRequestPassesOnlyTheRemainingTimeToTransport() {
        ScriptedConnection connection = ScriptedConnection.timeoutOnProfileResult();
        List<Duration> connectTimeouts = new ArrayList<>();
        DefaultRemoteTaskCageClient client = new DefaultRemoteTaskCageClient(
                options(),
                (options, timeout) -> {
                    connectTimeouts.add(timeout);
                    return connection;
                });

        assertThrows(
                TaskCageConnectionException.class,
                () -> client.getProfileResult(TASK_ID, Duration.ofMillis(100)));

        assertEquals(1, connectTimeouts.size());
        assertWithin(connectTimeouts.get(0), Duration.ofMillis(100));
        assertEquals(2, connection.readTimeouts.size());
        connection.readTimeouts.forEach(timeout -> assertWithin(timeout, Duration.ofMillis(100)));
    }

    @Test
    void tlsTimeoutConversionDoesNotOverflowForLargeDurations() {
        assertEquals(1, TlsFrameConnection.timeoutMillis(Duration.ofNanos(1)));
        assertEquals(
                Integer.MAX_VALUE,
                TlsFrameConnection.timeoutMillis(Duration.ofNanos(Long.MAX_VALUE)));
    }

    @Test
    void timeoutAwareResultRequestAlsoBoundsWaitingForTheSerializedConnection() throws Exception {
        ScriptedConnection connection = ScriptedConnection.blockOnProfileResult();
        DefaultRemoteTaskCageClient client = client(connection);
        AtomicReference<Throwable> blockingFailure = new AtomicReference<>();
        Thread blockingRequest = new Thread(() -> {
            try {
                client.getProfileResult(TASK_ID);
            } catch (Throwable exception) {
                blockingFailure.set(exception);
            }
        });
        blockingRequest.start();
        assertTrue(connection.profileReadStarted.await(1, TimeUnit.SECONDS));

        long startedAt = System.nanoTime();
        assertThrows(
                TaskCageConnectionException.class,
                () -> client.getProfileResult(TASK_ID, Duration.ofMillis(25)));
        long elapsedNanos = System.nanoTime() - startedAt;

        assertTrue(elapsedNanos < Duration.ofSeconds(1).toNanos());
        connection.releaseProfileRead.countDown();
        blockingRequest.join(Duration.ofSeconds(1).toMillis());
        assertTrue(!blockingRequest.isAlive());
        assertTrue(blockingFailure.get() instanceof TaskCageConnectionException);
    }

    @Test
    void successfulDownloadIgnoresTheOldPredictablePartialName() throws Exception {
        Path destination = directory.resolve("result.bin");
        Path predictable = directory.resolve("result.bin.taskcage-part");
        Files.writeString(predictable, "sentinel");
        DefaultRemoteTaskCageClient client = client(ScriptedConnection.successfulDownload());

        client.download(artifact(), destination);

        assertArrayEquals(CONTENT, Files.readAllBytes(destination));
        assertEquals("sentinel", Files.readString(predictable));
        assertNoUniquePartFiles();
    }

    @Test
    void predictablePartialSymlinkCannotTruncateItsTarget() throws Exception {
        Path destination = directory.resolve("result.bin");
        Path victim = directory.resolve("victim.bin");
        Path predictable = directory.resolve("result.bin.taskcage-part");
        Files.writeString(victim, "do-not-touch");
        try {
            Files.createSymbolicLink(predictable, victim.getFileName());
        } catch (IOException | UnsupportedOperationException | SecurityException exception) {
            Assumptions.assumeTrue(false, "symbolic links are unavailable: " + exception.getMessage());
        }
        DefaultRemoteTaskCageClient client = client(ScriptedConnection.successfulDownload());

        client.download(artifact(), destination);

        assertEquals("do-not-touch", Files.readString(victim));
        assertTrue(Files.isSymbolicLink(predictable));
        assertArrayEquals(CONTENT, Files.readAllBytes(destination));
        assertNoUniquePartFiles();
    }

    @Test
    void failedDownloadDeletesItsUniquePartialFile() throws Exception {
        Path destination = directory.resolve("result.bin");
        DefaultRemoteTaskCageClient client =
                client(ScriptedConnection.failAfterFirstDownloadChunk());

        assertThrows(TaskCageConnectionException.class, () -> client.download(artifact(), destination));

        assertTrue(Files.notExists(destination));
        assertNoUniquePartFiles();
    }

    @Test
    void failedAtomicMoveDeletesItsUniquePartialFile() throws Exception {
        Path destination = directory.resolve("result.bin");
        Files.createDirectory(destination);
        Files.writeString(destination.resolve("keep.txt"), "keep");
        DefaultRemoteTaskCageClient client = client(ScriptedConnection.successfulDownload());

        assertThrows(IOException.class, () -> client.download(artifact(), destination));

        assertTrue(Files.isDirectory(destination));
        assertEquals("keep", Files.readString(destination.resolve("keep.txt")));
        assertNoUniquePartFiles();
    }

    private DefaultRemoteTaskCageClient client(ScriptedConnection connection) {
        return new DefaultRemoteTaskCageClient(options(), (options, timeout) -> connection);
    }

    private RemoteConnectionOptions options() {
        return RemoteConnectionOptions.builder(
                        URI.create("taskcage+tls://taskcage.internal:7443"),
                        ServiceCredentials.of("document-worker", Secret.of("fixture-secret")))
                .connectTimeout(Duration.ofSeconds(3))
                .requestTimeout(Duration.ofSeconds(30))
                .build();
    }

    private ManagedOutputArtifact artifact() throws Exception {
        MessageDigest digest = MessageDigest.getInstance("SHA-256");
        return new ManagedOutputArtifact(
                ARTIFACT_ID,
                new Sha256Digest("sha256:" + HexFormat.of().formatHex(digest.digest(CONTENT))),
                CONTENT.length,
                "application/octet-stream",
                Instant.parse("2026-08-23T00:00:00Z"));
    }

    private void assertNoUniquePartFiles() throws IOException {
        try (var paths = Files.list(directory)) {
            assertTrue(paths.noneMatch(path -> path.getFileName().toString().matches(
                    "\\.result\\.bin\\.taskcage-[0-9a-f-]+\\.part")));
        }
    }

    private static void assertWithin(Duration actual, Duration limit) {
        assertTrue(!actual.isZero() && !actual.isNegative());
        assertTrue(actual.compareTo(limit) <= 0, () -> actual + " exceeds " + limit);
    }

    private static final class ScriptedConnection implements RemoteFrameConnection {
        private final boolean timeoutOnProfileResult;
        private final boolean failAfterFirstDownloadChunk;
        private final boolean blockOnProfileResult;
        private final List<Duration> readTimeouts = new ArrayList<>();
        private final CountDownLatch profileReadStarted = new CountDownLatch(1);
        private final CountDownLatch releaseProfileRead = new CountDownLatch(1);
        private byte[] request;
        private int chunkReads;

        private ScriptedConnection(
                boolean timeoutOnProfileResult,
                boolean failAfterFirstDownloadChunk,
                boolean blockOnProfileResult) {
            this.timeoutOnProfileResult = timeoutOnProfileResult;
            this.failAfterFirstDownloadChunk = failAfterFirstDownloadChunk;
            this.blockOnProfileResult = blockOnProfileResult;
        }

        static ScriptedConnection timeoutOnProfileResult() {
            return new ScriptedConnection(true, false, false);
        }

        static ScriptedConnection blockOnProfileResult() {
            return new ScriptedConnection(false, false, true);
        }

        static ScriptedConnection successfulDownload() {
            return new ScriptedConnection(false, false, false);
        }

        static ScriptedConnection failAfterFirstDownloadChunk() {
            return new ScriptedConnection(false, true, false);
        }

        @Override
        public void write(byte[] payload) {
            request = payload.clone();
        }

        @Override
        public byte[] read(Duration timeout) throws IOException {
            readTimeouts.add(timeout);
            JsonNode decoded = MAPPER.readTree(request);
            String type = decoded.path("type").asText();
            if ("getProfileResult".equals(type) && timeoutOnProfileResult) {
                throw new SocketTimeoutException("simulated Remote result timeout");
            }
            if ("getProfileResult".equals(type) && blockOnProfileResult) {
                profileReadStarted.countDown();
                try {
                    if (!releaseProfileRead.await(2, TimeUnit.SECONDS)) {
                        throw new SocketTimeoutException("blocking Remote result was not released");
                    }
                } catch (InterruptedException exception) {
                    Thread.currentThread().interrupt();
                    throw new IOException("interrupted blocking Remote result", exception);
                }
                throw new IOException("released blocking Remote result");
            }
            if ("readArtifactChunk".equals(type)
                    && failAfterFirstDownloadChunk
                    && chunkReads++ > 0) {
                throw new IOException("simulated interrupted download");
            }
            return switch (type) {
                case "authenticate" -> authenticated(decoded);
                case "getCapabilities" -> capabilities(decoded);
                case "readArtifactChunk" -> artifactChunk(decoded);
                default -> throw new IOException("unexpected Remote request: " + type);
            };
        }

        @Override
        public void close() {}

        private byte[] artifactChunk(JsonNode decoded) throws IOException {
            ObjectNode response = response(decoded, "artifactChunk");
            ObjectNode payload = response.putObject("payload");
            payload.put("artifactId", ARTIFACT_ID.toString());
            long offset = decoded.path("payload").path("offset").asLong();
            byte[] bytes;
            boolean finished;
            if (failAfterFirstDownloadChunk) {
                bytes = java.util.Arrays.copyOfRange(CONTENT, (int) offset, (int) offset + 2);
                finished = false;
            } else {
                bytes = CONTENT;
                finished = true;
            }
            payload.put("offset", offset);
            payload.put("dataBase64", Base64.getEncoder().encodeToString(bytes));
            payload.put("nextOffset", offset + bytes.length);
            payload.put("finished", finished);
            return MAPPER.writeValueAsBytes(response);
        }

        private static byte[] authenticated(JsonNode decoded) throws IOException {
            ObjectNode response = response(decoded, "authenticated");
            ObjectNode payload = response.putObject("payload");
            payload.put("principal", "document-worker");
            payload.put("sessionExpiresAt", "2026-08-23T00:00:00Z");
            return MAPPER.writeValueAsBytes(response);
        }

        private static byte[] capabilities(JsonNode decoded) throws IOException {
            ObjectNode response = response(decoded, "capabilities");
            ObjectNode payload = response.putObject("payload");
            payload.put("daemonVersion", "0.5.0");
            payload.putArray("remoteProtocolVersions").add(1);
            payload.put("maxFrameBytes", 1_048_576);
            payload.putArray("artifactModes").add("MANAGED_TRANSFER");
            payload.put("maxArtifactBytes", 104_857_600);
            payload.put("maxArtifactChunkBytes", 780_000);
            payload.put("artifactRetentionSeconds", 600);
            return MAPPER.writeValueAsBytes(response);
        }

        private static ObjectNode response(JsonNode request, String type) {
            ObjectNode response = MAPPER.createObjectNode();
            response.put("remoteProtocolVersion", 1);
            response.put("requestId", request.path("requestId").asText());
            response.put("type", type);
            return response;
        }
    }
}
