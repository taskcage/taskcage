package org.taskcage.binding.ffmpeg;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.MessageDigest;
import java.time.Duration;
import java.util.HexFormat;
import java.util.UUID;
import org.junit.jupiter.api.Test;
import org.taskcage.sdk.ArtifactPath;
import org.taskcage.sdk.LocalInputArtifact;
import org.taskcage.sdk.ProfileIdentity;
import org.taskcage.sdk.ProfileOutcome;
import org.taskcage.sdk.PublishedArtifact;
import org.taskcage.sdk.Sha256Digest;
import org.taskcage.sdk.TaskCageClient;
import org.taskcage.sdk.TaskCageClientConfig;
import org.taskcage.sdk.TerminationReason;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertTrue;

class FfmpegAudioToWavDaemonTest {
    private static final ProfileIdentity PROFILE =
            new ProfileIdentity("ffmpeg-audio-to-wav", "1.0.0");

    @Test
    void convertsAudioThroughTheTypedBindingAndPublishesVerifiedWav() throws Exception {
        InputArtifact input = createInput(sineWave());
        try (TaskCageClient client = client()) {
            assertTrue(client.capabilities().protocolVersions().contains(2));

            FfmpegAudioToWavResult result = FfmpegAudioToWavBinding.using(client)
                    .run(
                            UUID.randomUUID(),
                            new FfmpegAudioToWavRequest(
                                    input.reference(),
                                    AudioSampleRate.HZ_16000,
                                    AudioChannels.MONO),
                            Duration.ofSeconds(30));

            FfmpegAudioToWavSuccess success =
                    assertInstanceOf(FfmpegAudioToWavSuccess.class, result);
            assertEquals(PROFILE, success.task().profile());
            assertEquals(ProfileOutcome.SUCCEEDED, success.task().profileOutcome());
            assertEquals(TerminationReason.EXITED, success.task().result().terminationReason());
            assertEquals(0, success.task().result().process().exitCode());

            PublishedArtifact audio = success.audio();
            assertEquals("audio/wav", audio.mediaType());
            assertEquals(
                    "tasks/" + success.task().taskId() + "/result.wav",
                    audio.path().value());
            Path output = artifactRoot().resolve(audio.path().value());
            byte[] wav = Files.readAllBytes(output);
            assertTrue(wav.length >= 44);
            assertArrayEquals(new byte[] {'R', 'I', 'F', 'F'}, slice(wav, 0, 4));
            assertArrayEquals(new byte[] {'W', 'A', 'V', 'E'}, slice(wav, 8, 12));
            assertEquals(16_000, littleEndianInt(wav, 24));
            assertEquals(1, littleEndianShort(wav, 22));
            assertEquals(audio.sizeBytes(), wav.length);
            assertEquals(audio.digest(), digest(wav));

            Files.delete(output);
            Files.delete(output.getParent());
        } finally {
            input.delete();
        }
    }

    private static InputArtifact createInput(byte[] contents) throws Exception {
        String directory = "jobs/ffmpeg-binding-e2e-" + UUID.randomUUID();
        ArtifactPath path = new ArtifactPath(directory + "/source.wav");
        Path file = artifactRoot().resolve(path.value());
        Files.createDirectories(file.getParent());
        Files.write(file, contents);
        return new InputArtifact(
                file,
                new LocalInputArtifact(path, digest(contents), contents.length));
    }

    private static byte[] sineWave() throws IOException {
        int sampleRate = 44_100;
        int sampleCount = sampleRate / 4;
        ByteArrayOutputStream output = new ByteArrayOutputStream(44 + sampleCount * 2);
        writeAscii(output, "RIFF");
        writeInt(output, 36 + sampleCount * 2);
        writeAscii(output, "WAVEfmt ");
        writeInt(output, 16);
        writeShort(output, 1);
        writeShort(output, 1);
        writeInt(output, sampleRate);
        writeInt(output, sampleRate * 2);
        writeShort(output, 2);
        writeShort(output, 16);
        writeAscii(output, "data");
        writeInt(output, sampleCount * 2);
        for (int index = 0; index < sampleCount; index++) {
            double phase = 2.0 * Math.PI * 440.0 * index / sampleRate;
            writeShort(output, (int) (Math.sin(phase) * 8_000));
        }
        return output.toByteArray();
    }

    private static void writeAscii(ByteArrayOutputStream output, String value) throws IOException {
        output.write(value.getBytes(java.nio.charset.StandardCharsets.US_ASCII));
    }

    private static void writeInt(ByteArrayOutputStream output, int value) {
        output.writeBytes(ByteBuffer.allocate(4)
                .order(ByteOrder.LITTLE_ENDIAN)
                .putInt(value)
                .array());
    }

    private static void writeShort(ByteArrayOutputStream output, int value) {
        output.writeBytes(ByteBuffer.allocate(2)
                .order(ByteOrder.LITTLE_ENDIAN)
                .putShort((short) value)
                .array());
    }

    private static int littleEndianInt(byte[] value, int offset) {
        return ByteBuffer.wrap(value, offset, 4).order(ByteOrder.LITTLE_ENDIAN).getInt();
    }

    private static int littleEndianShort(byte[] value, int offset) {
        return Short.toUnsignedInt(
                ByteBuffer.wrap(value, offset, 2).order(ByteOrder.LITTLE_ENDIAN).getShort());
    }

    private static byte[] slice(byte[] value, int from, int to) {
        return java.util.Arrays.copyOfRange(value, from, to);
    }

    private static Sha256Digest digest(byte[] contents) throws Exception {
        byte[] value = MessageDigest.getInstance("SHA-256").digest(contents);
        return new Sha256Digest("sha256:" + HexFormat.of().formatHex(value));
    }

    private static TaskCageClient client() {
        return TaskCageClient.connect(TaskCageClientConfig.builder()
                .socketPath(Path.of(System.getenv("TASKCAGE_SOCKET")))
                .build());
    }

    private static Path artifactRoot() {
        return Path.of(System.getenv("TASKCAGE_ARTIFACT_ROOT"));
    }

    private record InputArtifact(Path file, LocalInputArtifact reference) {
        private void delete() throws IOException {
            Files.deleteIfExists(file);
            Files.deleteIfExists(file.getParent());
        }
    }
}
