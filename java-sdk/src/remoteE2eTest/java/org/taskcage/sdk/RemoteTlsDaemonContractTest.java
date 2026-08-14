package org.taskcage.sdk;

import java.net.URI;
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
