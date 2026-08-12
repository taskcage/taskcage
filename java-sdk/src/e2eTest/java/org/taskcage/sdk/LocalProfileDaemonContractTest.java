package org.taskcage.sdk;

import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.MessageDigest;
import java.time.Duration;
import java.util.HexFormat;
import java.util.Map;
import java.util.Set;
import java.util.UUID;
import java.util.stream.Collectors;
import java.util.stream.Stream;
import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

class LocalProfileDaemonContractTest {
    private static final ProfileIdentity FILE_COPY = new ProfileIdentity("file-copy", "1.0.0");

    @Test
    void executesProfileAndPublishesArtifactAfterCleanup() throws Exception {
        byte[] content = "TaskCage Java Profile E2E\n".getBytes(StandardCharsets.UTF_8);
        InputArtifact input = createInput(content);
        UUID clientRequestId = UUID.randomUUID();

        try (TaskCageClient client = client()) {
            assertTrue(client.capabilities().protocolVersions().contains(2));

            ProfileTaskHandle handle = client.submitProfileHandle(clientRequestId, request(input, "archive"));
            FinishedProfileTaskSnapshot finished = handle.await(Duration.ofSeconds(10));

            assertEquals(ProfileOutcome.SUCCEEDED, finished.profileOutcome());
            assertEquals(FILE_COPY, finished.profile());
            assertEquals(TerminationReason.EXITED, finished.result().terminationReason());
            assertEquals(0, finished.result().process().exitCode());
            assertEquals(finished, client.getProfileResult(finished.taskId()));

            FinishedTaskSnapshot rawSnapshot = (FinishedTaskSnapshot) client.getTask(finished.taskId());
            assertEquals(TerminationReason.EXITED, rawSnapshot.result().terminationReason());

            PublishedArtifact artifact = finished.artifacts().get("result");
            assertEquals(input.digest(), artifact.digest());
            assertEquals(content.length, artifact.sizeBytes());
            assertEquals("text/plain", artifact.mediaType());
            assertEquals(
                    "tasks/" + finished.taskId() + "/result.txt",
                    artifact.path().value());
            assertArrayEquals(content, Files.readAllBytes(artifactRoot().resolve(artifact.path().value())));

            FinishedProfileTaskSnapshot retried = client.run(
                    clientRequestId, request(input, "archive"), Duration.ofSeconds(10));
            assertEquals(finished, retried);

            TaskCageDaemonException conflict = assertThrows(
                    TaskCageDaemonException.class,
                    () -> client.submitProfile(clientRequestId, request(input, "changed-label")));
            assertEquals("IDEMPOTENCY_CONFLICT", conflict.code());
            assertFalse(conflict.retryable());

            deletePublishedArtifact(artifact);
        } finally {
            input.delete();
        }
    }

    @Test
    void rejectsDigestMismatchWithoutPublishingArtifact() throws Exception {
        byte[] content = "digest mismatch\n".getBytes(StandardCharsets.UTF_8);
        InputArtifact input = createInput(content);
        Set<String> publishedBefore = publishedTaskDirectories();
        Sha256Digest wrongDigest = digest("different bytes".getBytes(StandardCharsets.UTF_8));
        ProfileRequest invalid = request(new InputArtifact(input.path(), input.file(), wrongDigest, content.length), "bad");

        try (TaskCageClient client = client()) {
            TaskCageDaemonException error = assertThrows(
                    TaskCageDaemonException.class,
                    () -> client.submitProfile(UUID.randomUUID(), invalid));
            assertEquals("ARTIFACT_DIGEST_MISMATCH", error.code());
            assertFalse(error.retryable());
            assertEquals(publishedBefore, publishedTaskDirectories());
        } finally {
            input.delete();
        }
    }

    private static ProfileRequest request(InputArtifact input, String label) {
        return new ProfileRequest(
                FILE_COPY,
                Map.of(
                        "source",
                        new LocalInputArtifact(input.path(), input.digest(), input.sizeBytes()),
                        "label",
                        new StringProfileInput(label),
                        "retain_metadata",
                        new BooleanProfileInput(true),
                        "priority",
                        new Int64ProfileInput(3)),
                ProfileResourceOverrides.builder()
                        .wallTimeLimit(Duration.ofSeconds(10))
                        .build());
    }

    private static InputArtifact createInput(byte[] content) throws Exception {
        String directory = "jobs/java-e2e-" + UUID.randomUUID();
        ArtifactPath path = new ArtifactPath(directory + "/source.txt");
        Path file = artifactRoot().resolve(path.value());
        Files.createDirectories(file.getParent());
        Files.write(file, content);
        return new InputArtifact(path, file, digest(content), content.length);
    }

    private static Sha256Digest digest(byte[] content) throws Exception {
        byte[] digest = MessageDigest.getInstance("SHA-256").digest(content);
        return new Sha256Digest("sha256:" + HexFormat.of().formatHex(digest));
    }

    private static Set<String> publishedTaskDirectories() throws Exception {
        Path tasks = artifactRoot().resolve("tasks");
        if (!Files.isDirectory(tasks)) {
            return Set.of();
        }
        try (Stream<Path> entries = Files.list(tasks)) {
            return entries
                    .filter(Files::isDirectory)
                    .map(path -> path.getFileName().toString())
                    .collect(Collectors.toUnmodifiableSet());
        }
    }

    private static void deletePublishedArtifact(PublishedArtifact artifact) throws Exception {
        Path file = artifactRoot().resolve(artifact.path().value());
        Files.deleteIfExists(file);
        Files.deleteIfExists(file.getParent());
    }

    private static TaskCageClient client() {
        return TaskCageClient.connect(TaskCageClientConfig.builder()
                .socketPath(Path.of(System.getenv("TASKCAGE_SOCKET")))
                .build());
    }

    private static Path artifactRoot() {
        return Path.of(System.getenv("TASKCAGE_ARTIFACT_ROOT"));
    }

    private record InputArtifact(ArtifactPath path, Path file, Sha256Digest digest, long sizeBytes) {
        private void delete() throws Exception {
            Files.deleteIfExists(file);
            Files.deleteIfExists(file.getParent());
        }
    }
}
