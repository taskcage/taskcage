package org.taskcage.sdk;

import java.net.URI;
import java.nio.charset.StandardCharsets;
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
import static org.junit.jupiter.api.Assertions.assertTrue;

/** Contract test for Java's TLS client against a daemon configured with an authorized FFmpeg Profile. */
class RemoteTlsDaemonContractTest {
    @Test
    void uploadsSubmitsAndDownloadsThroughTheRemoteDaemon() throws Exception {
        byte[] sourceBytes = wavFixture();
        Path source = Files.createTempFile("taskcage-remote-source-", ".wav");
        Path destination = Files.createTempFile("taskcage-remote-output-", ".wav");
        try {
            Files.write(source, sourceBytes);
            try (RemoteTaskCageClient client = RemoteTaskCageClient.connect(options())) {
                assertTrue(client.capabilities().supportsManagedTransfer());
                RemoteArtifactUpload uploaded = client.upload(source, "audio/wav");
                RemoteProfileTask task = client.submitProfile(new RemoteProfileRequest(
                        new ProfileIdentity("ffmpeg-audio-to-wav", "1.0.0"),
                        Map.of("source", uploaded.asInput(), "sample_rate_hz", new RemoteInt64Input(8000),
                                "channels", new RemoteInt64Input(1))));
                RemoteProfileTaskSnapshot snapshot = awaitFinished(client, task.taskId());
                FinishedRemoteProfileTaskSnapshot finished = (FinishedRemoteProfileTaskSnapshot) snapshot;
                assertEquals(ProfileOutcome.SUCCEEDED, finished.profileOutcome());
                ManagedOutputArtifact output = finished.artifacts().get("audio");
                client.download(output, destination);
                assertArrayEquals("RIFF".getBytes(StandardCharsets.US_ASCII),
                        java.util.Arrays.copyOf(Files.readAllBytes(destination), 4));
            }
        } finally { Files.deleteIfExists(source); Files.deleteIfExists(destination); }
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

    private static byte[] wavFixture() { return new byte[] {'R','I','F','F',36,0,0,0,'W','A','V','E','f','m','t',' ',16,0,0,0,1,0,1,0,64,31,0,0,-128,62,0,0,2,0,16,0,'d','a','t','a',0,0,0,0}; }
}
