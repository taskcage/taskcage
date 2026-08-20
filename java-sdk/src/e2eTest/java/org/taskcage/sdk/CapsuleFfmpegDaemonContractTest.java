package org.taskcage.sdk;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.MessageDigest;
import java.time.Duration;
import java.util.HexFormat;
import java.util.Map;
import java.util.UUID;
import org.junit.jupiter.api.Test;

/** Exercises the generic Capsule API against the Compose-imported FFmpeg Capsule. */
class CapsuleFfmpegDaemonContractTest {
    private static final CapsuleIdentity CAPSULE =
            new CapsuleIdentity("ffmpeg-audio-to-wav", "1.0.0");

    @Test
    void executesTheInstalledFfmpegCapsuleAndPublishesVerifiedOutput() throws Exception {
        InputArtifact input = createInput(wave());
        try (TaskCageClient client = TaskCageClient.connect(TaskCageClientConfig.builder()
                .socketPath(Path.of(System.getenv("TASKCAGE_SOCKET")))
                .build())) {
            CapsuleExecutionResult result = CapsuleRunner.external(client).execute(
                    UUID.randomUUID(),
                    CapsuleRequest.builder(CAPSULE)
                            .artifact("source", input.reference())
                            .int64("sample_rate_hz", 16_000)
                            .int64("channels", 1)
                            .build(),
                    Duration.ofSeconds(30));

            assertEquals(ProfileOutcome.SUCCEEDED, result.outcome());
            assertEquals(CAPSULE.name(), result.profileTask().profile().name());
            assertEquals(CAPSULE.version(), result.profileTask().profile().version());
            assertEquals(TerminationReason.EXITED, result.execution().terminationReason());
            assertEquals(0, result.execution().process().exitCode());
            assertTrue(result.cleanupConfirmed());

            PublishedArtifact audio = result.profileTask().artifacts().get("audio");
            assertEquals("audio/wav", audio.mediaType());
            Path output = artifactRoot().resolve(audio.path().value());
            byte[] bytes = Files.readAllBytes(output);
            assertTrue(bytes.length >= 44);
            assertArrayEquals(new byte[] {'R', 'I', 'F', 'F'}, java.util.Arrays.copyOfRange(bytes, 0, 4));
            assertArrayEquals(new byte[] {'W', 'A', 'V', 'E'}, java.util.Arrays.copyOfRange(bytes, 8, 12));
            assertEquals(audio.sizeBytes(), bytes.length);
            assertEquals(audio.digest(), digest(bytes));

            Files.delete(output);
            Files.delete(output.getParent());
        } finally {
            input.delete();
        }
    }

    private static InputArtifact createInput(byte[] bytes) throws Exception {
        String directory = "jobs/capsule-ffmpeg-e2e-" + UUID.randomUUID();
        ArtifactPath path = new ArtifactPath(directory + "/source.wav");
        Path file = artifactRoot().resolve(path.value());
        Files.createDirectories(file.getParent());
        Files.write(file, bytes);
        return new InputArtifact(
                file,
                new LocalInputArtifact(path, digest(bytes), bytes.length));
    }

    private static byte[] wave() {
        int samples = 8_000;
        ByteBuffer buffer = ByteBuffer.allocate(44 + samples * 2).order(ByteOrder.LITTLE_ENDIAN);
        buffer.put("RIFF".getBytes(java.nio.charset.StandardCharsets.US_ASCII));
        buffer.putInt(36 + samples * 2);
        buffer.put("WAVEfmt ".getBytes(java.nio.charset.StandardCharsets.US_ASCII));
        buffer.putInt(16).putShort((short) 1).putShort((short) 1).putInt(8_000);
        buffer.putInt(16_000).putShort((short) 2).putShort((short) 16);
        buffer.put("data".getBytes(java.nio.charset.StandardCharsets.US_ASCII)).putInt(samples * 2);
        for (int index = 0; index < samples; index++) {
            buffer.putShort((short) (Math.sin(2 * Math.PI * 440 * index / 8_000) * 8_000));
        }
        return buffer.array();
    }

    private static Sha256Digest digest(byte[] bytes) throws Exception {
        return new Sha256Digest("sha256:" + HexFormat.of().formatHex(
                MessageDigest.getInstance("SHA-256").digest(bytes)));
    }

    private static Path artifactRoot() {
        return Path.of(System.getenv("TASKCAGE_ARTIFACT_ROOT"));
    }

    private record InputArtifact(Path file, LocalInputArtifact reference) {
        private void delete() throws Exception {
            Files.deleteIfExists(file);
            Files.deleteIfExists(file.getParent());
        }
    }
}
