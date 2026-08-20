package org.taskcage.sdk;

import java.net.URI;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.KeyStore;
import java.security.cert.CertificateFactory;
import java.time.Duration;
import java.util.Map;
import javax.net.ssl.SSLContext;
import javax.net.ssl.TrustManagerFactory;
import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

/** Contract test for Java's TLS client against a daemon configured with the opt-in file-copy Profile. */
class RemoteTlsDaemonContractTest {
    private static final CapsuleIdentity FFMPEG_CAPSULE =
            new CapsuleIdentity("ffmpeg-audio-to-wav", "1.0.0");

    @Test
    void uploadsSubmitsAndDownloadsThroughTheRemoteDaemon() throws Exception {
        byte[] sourceBytes = new byte[1_600_003];
        for (int index = 0; index < sourceBytes.length; index++) sourceBytes[index] = (byte) (index % 251);
        Path source = Files.createTempFile("taskcage-remote-source-", ".txt");
        Path destination = Files.createTempFile("taskcage-remote-output-", ".txt");
        try {
            Files.write(source, sourceBytes);
            try (RemoteTaskCageClient client = RemoteTaskCageClient.connect(options())) {
                assertTrue(client.capabilities().supportsManagedTransfer());
                RemoteArtifactUpload uploaded = client.upload(source, "text/plain");
                RemoteProfileTask task = client.submitProfile(new RemoteProfileRequest(
                        new ProfileIdentity("file-copy", "1.0.0"),
                        Map.of("source", uploaded.asInput(), "label", new RemoteStringInput("remote"),
                                "retain_metadata", new RemoteBooleanInput(true), "priority", new RemoteInt64Input(1))));
                RemoteProfileTaskSnapshot snapshot = awaitFinished(client, task.taskId());
                FinishedRemoteProfileTaskSnapshot finished = (FinishedRemoteProfileTaskSnapshot) snapshot;
                assertEquals(ProfileOutcome.SUCCEEDED, finished.profileOutcome());
                ManagedOutputArtifact output = finished.artifacts().get("result");
                client.download(output, destination);
                assertArrayEquals(sourceBytes, Files.readAllBytes(destination));
            }
        } finally { Files.deleteIfExists(source); Files.deleteIfExists(destination); }
    }

    @Test
    void rejectsInvalidServiceAccountSecret() throws Exception {
        RemoteConnectionOptions invalid = RemoteConnectionOptions.builder(
                        URI.create(System.getenv("TASKCAGE_REMOTE_ENDPOINT")),
                        ServiceCredentials.of(System.getenv("TASKCAGE_REMOTE_CLIENT_ID"), Secret.of("not-the-daemon-secret")))
                .sslContext(trustContext(Path.of(System.getenv("TASKCAGE_REMOTE_CA_PEM"))))
                .build();

        try (RemoteTaskCageClient client = RemoteTaskCageClient.connect(invalid)) {
            TaskCageDaemonException exception = assertThrows(TaskCageDaemonException.class, client::capabilities);
            assertEquals("AUTHENTICATION_FAILED", exception.code());
            assertEquals(false, exception.retryable());
        }
    }

    @Test
    void executesFfmpegCapsuleThroughTlsAndDownloadsThePublishedArtifact() throws Exception {
        Path source = Files.createTempFile("taskcage-remote-capsule-source-", ".wav");
        Path destination = Files.createTempFile("taskcage-remote-capsule-output-", ".wav");
        try {
            Files.write(source, wave(8_000));
            try (RemoteTaskCageClient client = RemoteTaskCageClient.connect(options())) {
                RemoteArtifactUpload uploaded = client.upload(source, "audio/wav");
                RemoteCapsuleExecutionResult result = RemoteCapsuleRunner.external(client).execute(
                        capsuleRequest(uploaded, ProfileResourceOverrides.none()), Duration.ofSeconds(20));

                assertEquals(ProfileOutcome.SUCCEEDED, result.outcome());
                assertEquals(TerminationReason.EXITED, result.execution().terminationReason());
                assertTrue(result.cleanupConfirmed());
                ManagedOutputArtifact output = result.profileTask().artifacts().get("audio");
                assertEquals("audio/wav", output.mediaType());
                client.download(output, destination);
                byte[] bytes = Files.readAllBytes(destination);
                assertArrayEquals(new byte[] {'R', 'I', 'F', 'F'}, java.util.Arrays.copyOfRange(bytes, 0, 4));
                assertArrayEquals(new byte[] {'W', 'A', 'V', 'E'}, java.util.Arrays.copyOfRange(bytes, 8, 12));
            }
        } finally {
            Files.deleteIfExists(source);
            Files.deleteIfExists(destination);
        }
    }

    @Test
    void reportsTimeoutForTheFfmpegCapsule() throws Exception {
        Path source = Files.createTempFile("taskcage-remote-capsule-limit-", ".wav");
        try {
            Files.write(source, wave(8_000));
            try (RemoteTaskCageClient client = RemoteTaskCageClient.connect(options())) {
                RemoteCapsuleRunner runner = RemoteCapsuleRunner.external(client);

                RemoteCapsuleExecutionResult timedOut = runner.execute(
                        capsuleRequest(
                                client.upload(source, "audio/wav"),
                                ProfileResourceOverrides.builder()
                                        .wallTimeLimit(Duration.ofMillis(1))
                                        .build()),
                        Duration.ofSeconds(20));
                assertEquals(ProfileOutcome.FAILED, timedOut.outcome());
                assertEquals(TerminationReason.TIMED_OUT, timedOut.execution().terminationReason());
                assertTrue(timedOut.profileTask().artifacts().isEmpty());

            }
        } finally {
            Files.deleteIfExists(source);
        }
    }

    @Test
    void reportsMemoryLimitForTheFfmpegCapsuleAndPublishesNoArtifact() throws Exception {
        Path source = Files.createTempFile("taskcage-remote-capsule-memory-", ".wav");
        try {
            Files.write(source, wave(8_000));
            try (RemoteTaskCageClient client = RemoteTaskCageClient.connect(options())) {
                RemoteCapsuleExecutionResult limited = RemoteCapsuleRunner.external(client).execute(
                        capsuleRequest(
                                client.upload(source, "audio/wav"),
                                ProfileResourceOverrides.builder()
                                        .memoryMaxBytes(4 * 1024L)
                                        .build()),
                        Duration.ofSeconds(20));

                assertEquals(ProfileOutcome.FAILED, limited.outcome());
                assertEquals(TerminationReason.MEMORY_LIMIT_EXCEEDED, limited.execution().terminationReason());
                assertTrue(limited.profileTask().artifacts().isEmpty());
                assertTrue(limited.cleanupConfirmed());
            }
        } finally {
            Files.deleteIfExists(source);
        }
    }

    @Test
    void reportsProcessLimitForTheFfmpegCapsuleAndPublishesNoArtifact() throws Exception {
        Path source = Files.createTempFile("taskcage-remote-capsule-pids-", ".wav");
        try {
            Files.write(source, wave(2_500_000));
            try (RemoteTaskCageClient client = RemoteTaskCageClient.connect(options())) {
                RemoteCapsuleExecutionResult limited = RemoteCapsuleRunner.external(client).execute(
                        capsuleRequest(
                                client.upload(source, "audio/wav"),
                                ProfileResourceOverrides.builder()
                                        .pidsMax(1)
                                        .build()),
                        Duration.ofSeconds(20));

                assertEquals(ProfileOutcome.FAILED, limited.outcome());
                assertEquals(
                        TerminationReason.PROCESS_LIMIT_EXCEEDED,
                        limited.execution().terminationReason());
                assertTrue(limited.profileTask().artifacts().isEmpty());
                assertTrue(limited.cleanupConfirmed());
            }
        } finally {
            Files.deleteIfExists(source);
        }
    }

    @Test
    void cancelsAnAcceptedFfmpegCapsuleAndObservesCleanupConfirmedFailure() throws Exception {
        Path source = Files.createTempFile("taskcage-remote-capsule-cancel-", ".wav");
        try {
            Files.write(source, wave(2_500_000));
            try (RemoteTaskCageClient client = RemoteTaskCageClient.connect(options())) {
                RemoteCapsuleTaskHandle handle = RemoteCapsuleRunner.external(client).submit(
                        capsuleRequest(client.upload(source, "audio/wav"), ProfileResourceOverrides.none()));

                TaskCancellation cancellation = handle.cancel();
                assertEquals(TerminationReason.CANCELLED, cancellation.terminationReason());
                RemoteCapsuleExecutionResult result = handle.await(Duration.ofSeconds(20));
                assertEquals(ProfileOutcome.FAILED, result.outcome());
                assertEquals(TerminationReason.CANCELLED, result.execution().terminationReason());
                assertTrue(result.profileTask().artifacts().isEmpty());
                assertTrue(result.cleanupConfirmed());
            }
        } finally {
            Files.deleteIfExists(source);
        }
    }

    private static RemoteCapsuleRequest capsuleRequest(
            RemoteArtifactUpload source, ProfileResourceOverrides overrides) {
        return new RemoteCapsuleRequest(
                FFMPEG_CAPSULE,
                new RemoteProfileRequest(
                        new ProfileIdentity(FFMPEG_CAPSULE.name(), FFMPEG_CAPSULE.version()),
                        Map.of(
                                "source", source.asInput(),
                                "sample_rate_hz", new RemoteInt64Input(16_000),
                                "channels", new RemoteInt64Input(1)),
                        overrides));
    }

    private static byte[] wave(int samples) {
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

    private static RemoteProfileTaskSnapshot awaitFinished(RemoteTaskCageClient client, java.util.UUID taskId) throws Exception {
        long deadline = System.nanoTime() + Duration.ofSeconds(20).toNanos();
        while (System.nanoTime() < deadline) {
            RemoteProfileTaskSnapshot snapshot = client.getProfileResult(taskId);
            if (snapshot instanceof FinishedRemoteProfileTaskSnapshot) return snapshot;
            Thread.sleep(50);
        }
        throw new AssertionError("Remote Profile Task did not finish");
    }

    private static RemoteConnectionOptions options() throws Exception {
        return RemoteConnectionOptions.builder(URI.create(System.getenv("TASKCAGE_REMOTE_ENDPOINT")),
                ServiceCredentials.of(System.getenv("TASKCAGE_REMOTE_CLIENT_ID"), Secret.of(System.getenv("TASKCAGE_REMOTE_SECRET"))))
                .sslContext(trustContext(Path.of(System.getenv("TASKCAGE_REMOTE_CA_PEM")))).build();
    }

    private static SSLContext trustContext(Path pem) throws Exception {
        java.security.cert.Certificate certificate;
        try (var input = Files.newInputStream(pem)) {
            certificate = CertificateFactory.getInstance("X.509").generateCertificate(input);
        }
        KeyStore store = KeyStore.getInstance(KeyStore.getDefaultType()); store.load(null); store.setCertificateEntry("taskcage", certificate);
        TrustManagerFactory managers = TrustManagerFactory.getInstance(TrustManagerFactory.getDefaultAlgorithm()); managers.init(store);
        SSLContext context = SSLContext.getInstance("TLS"); context.init(null, managers.getTrustManagers(), null); return context;
    }

}
