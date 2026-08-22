package org.taskcage.sdk;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.IOException;
import java.nio.file.Path;
import java.time.Duration;
import java.time.Instant;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.UUID;
import org.junit.jupiter.api.Test;

final class RemoteCapsuleRunnerTest {
    private static final CapsuleIdentity CAPSULE = new CapsuleIdentity("test-capsule", "1.0.0");
    private static final RemoteCapsuleRequest REQUEST = new RemoteCapsuleRequest(
            CAPSULE,
            new RemoteProfileRequest(
                    new ProfileIdentity(CAPSULE.name(), CAPSULE.version()),
                    Map.of("value", new RemoteInt64Input(1))));
    private static final RemoteCapsuleFileRequest FILE_REQUEST = RemoteCapsuleFileRequest.builder(
                    CAPSULE.name(), CAPSULE.version())
            .inputFile("source", Path.of("input.bin"), "application/octet-stream")
            .outputFile("result", Path.of("output.bin"))
            .build();

    @Test
    void rejectsInvalidWaitTimeoutBeforeSubmitting() {
        RecordingRemoteClient client = new RecordingRemoteClient();
        RemoteCapsuleRunner runner = RemoteCapsuleRunner.external(client);

        assertInvalidWaitTimeouts(timeout -> runner.execute(REQUEST, timeout));

        assertTrue(client.operations.isEmpty(), "invalid waitTimeout must not call the remote client");
    }

    @Test
    void rejectsInvalidWaitTimeoutBeforeSubmittingWithIdempotencyKey() {
        RecordingRemoteClient client = new RecordingRemoteClient();
        RemoteCapsuleRunner runner = RemoteCapsuleRunner.external(client);
        UUID clientRequestId = UUID.randomUUID();

        assertInvalidWaitTimeouts(timeout -> runner.execute(clientRequestId, REQUEST, timeout));

        assertTrue(client.operations.isEmpty(), "invalid waitTimeout must not call the remote client");
    }

    @Test
    void rejectsInvalidWaitTimeoutBeforeUploadingFile() {
        RecordingRemoteClient client = new RecordingRemoteClient();
        RemoteCapsuleRunner runner = RemoteCapsuleRunner.external(client);

        assertInvalidWaitTimeouts(timeout -> runner.execute(FILE_REQUEST, timeout));

        assertTrue(client.operations.isEmpty(), "invalid waitTimeout must not upload or submit");
    }

    @Test
    void recoversUploadAndSubmissionWithCallerOwnedIds() throws Exception {
        RecoverableRemoteClient client = new RecoverableRemoteClient();
        RemoteCapsuleRunner runner = RemoteCapsuleRunner.external(client);
        UUID clientArtifactId = UUID.randomUUID();
        UUID clientRequestId = UUID.randomUUID();

        client.loseNextUploadResponse = true;
        assertThrows(
                TaskCageConnectionException.class,
                () -> runner.upload(clientArtifactId, FILE_REQUEST));
        RemoteArtifactUpload upload = runner.upload(clientArtifactId, FILE_REQUEST);

        client.loseNextSubmitResponse = true;
        assertThrows(
                TaskCageConnectionException.class,
                () -> runner.submit(clientRequestId, FILE_REQUEST, upload));
        RemoteCapsuleTaskHandle recovered = runner.submit(clientRequestId, FILE_REQUEST, upload);

        assertEquals(2, client.uploadCalls);
        assertEquals(clientArtifactId, client.clientArtifactId);
        assertEquals(2, client.submitCalls);
        assertEquals(clientRequestId, client.clientRequestId);
        assertEquals(client.task.taskId(), recovered.taskId());
        assertEquals(upload.asInput(), client.submittedRequest.inputs().get(FILE_REQUEST.inputSlot()));
    }

    @Test
    void retriesDownloadWithoutSubmittingAnotherTask() throws Exception {
        RecoverableRemoteClient client = new RecoverableRemoteClient();
        RemoteCapsuleRunner runner = RemoteCapsuleRunner.external(client);
        RemoteArtifactUpload upload = runner.upload(UUID.randomUUID(), FILE_REQUEST);
        RemoteCapsuleTaskHandle task = runner.submit(UUID.randomUUID(), FILE_REQUEST, upload);
        RemoteCapsuleExecutionResult result = task.await(Duration.ofSeconds(1));

        client.failNextDownload = true;
        assertThrows(IOException.class, () -> runner.download(FILE_REQUEST, result));
        runner.download(FILE_REQUEST, result);

        assertEquals(1, client.uploadCalls);
        assertEquals(1, client.submitCalls);
        assertEquals(2, client.downloadCalls);
        assertEquals(client.output, client.downloadedArtifact);
        assertEquals(FILE_REQUEST.outputFile(), client.downloadDestination);
    }

    private static void assertInvalidWaitTimeouts(TimeoutInvocation invocation) {
        assertThrows(NullPointerException.class, () -> invocation.run(null));
        assertThrows(IllegalArgumentException.class, () -> invocation.run(Duration.ZERO));
        assertThrows(IllegalArgumentException.class, () -> invocation.run(Duration.ofNanos(-1)));
        assertThrows(
                IllegalArgumentException.class,
                () -> invocation.run(Duration.ofSeconds(Long.MAX_VALUE)));
    }

    @FunctionalInterface
    private interface TimeoutInvocation {
        void run(Duration timeout) throws Exception;
    }

    private static final class RecordingRemoteClient implements RemoteTaskCageClient {
        private final List<String> operations = new ArrayList<>();

        @Override
        public RemoteCapabilities capabilities() {
            return unexpected("capabilities");
        }

        @Override
        public RemoteArtifactUpload upload(Path source, String mediaType) throws IOException {
            return unexpected("upload");
        }

        @Override
        public RemoteArtifactUpload upload(UUID clientArtifactId, Path source, String mediaType)
                throws IOException {
            return unexpected("uploadWithIdempotencyKey");
        }

        @Override
        public void download(ManagedOutputArtifact artifact, Path destination) throws IOException {
            unexpected("download");
        }

        @Override
        public RemoteProfileTask submitProfile(RemoteProfileRequest request) {
            return unexpected("submitProfile");
        }

        @Override
        public RemoteProfileTask submitProfile(UUID clientRequestId, RemoteProfileRequest request) {
            return unexpected("submitProfileWithIdempotencyKey");
        }

        @Override
        public RemoteProfileTaskSnapshot getProfileResult(UUID taskId) {
            return unexpected("getProfileResult");
        }

        @Override
        public TaskCancellation cancelTask(UUID taskId) {
            return unexpected("cancelTask");
        }

        @Override
        public void close() {
            unexpected("close");
        }

        private <T> T unexpected(String operation) {
            operations.add(operation);
            throw new AssertionError("unexpected remote client operation: " + operation);
        }
    }

    private static final class RecoverableRemoteClient implements RemoteTaskCageClient {
        private final UUID artifactId = UUID.randomUUID();
        private final UUID taskId = UUID.randomUUID();
        private final Sha256Digest digest = new Sha256Digest(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        private final RemoteArtifactUpload upload =
                new RemoteArtifactUpload(artifactId, digest, 1, Instant.parse("2026-08-23T00:00:00Z"));
        private final ManagedOutputArtifact output = new ManagedOutputArtifact(
                UUID.randomUUID(),
                digest,
                1,
                "application/octet-stream",
                Instant.parse("2026-08-23T00:00:00Z"));
        private final RemoteProfileTask task = new RemoteProfileTask(
                taskId,
                new ProfileIdentity(CAPSULE.name(), CAPSULE.version()),
                ResourceBudget.safeDefaults());
        private final FinishedRemoteProfileTaskSnapshot finished = finished(taskId, output);

        private UUID clientArtifactId;
        private Path uploadSource;
        private String uploadMediaType;
        private UUID clientRequestId;
        private RemoteProfileRequest submittedRequest;
        private ManagedOutputArtifact downloadedArtifact;
        private Path downloadDestination;
        private int uploadCalls;
        private int submitCalls;
        private int downloadCalls;
        private boolean loseNextUploadResponse;
        private boolean loseNextSubmitResponse;
        private boolean failNextDownload;

        @Override
        public RemoteCapabilities capabilities() {
            throw new AssertionError("capabilities must not be called");
        }

        @Override
        public RemoteArtifactUpload upload(Path source, String mediaType) {
            throw new AssertionError("the file stage must use a caller-owned clientArtifactId");
        }

        @Override
        public RemoteArtifactUpload upload(UUID id, Path source, String mediaType) {
            uploadCalls++;
            if (clientArtifactId == null) {
                clientArtifactId = id;
                uploadSource = source;
                uploadMediaType = mediaType;
            } else {
                assertEquals(clientArtifactId, id);
                assertEquals(uploadSource, source);
                assertEquals(uploadMediaType, mediaType);
            }
            if (loseNextUploadResponse) {
                loseNextUploadResponse = false;
                throw lostResponse("upload");
            }
            return upload;
        }

        @Override
        public void download(ManagedOutputArtifact artifact, Path destination) throws IOException {
            downloadCalls++;
            downloadedArtifact = artifact;
            downloadDestination = destination;
            if (failNextDownload) {
                failNextDownload = false;
                throw new IOException("simulated download failure");
            }
        }

        @Override
        public RemoteProfileTask submitProfile(RemoteProfileRequest request) {
            throw new AssertionError("the file stage must use a caller-owned clientRequestId");
        }

        @Override
        public RemoteProfileTask submitProfile(UUID id, RemoteProfileRequest request) {
            submitCalls++;
            if (clientRequestId == null) {
                clientRequestId = id;
                submittedRequest = request;
            } else {
                assertEquals(clientRequestId, id);
                assertEquals(submittedRequest, request);
            }
            if (loseNextSubmitResponse) {
                loseNextSubmitResponse = false;
                throw lostResponse("submission");
            }
            return task;
        }

        @Override
        public RemoteProfileTaskSnapshot getProfileResult(UUID requestedTaskId) {
            assertEquals(taskId, requestedTaskId);
            return finished;
        }

        @Override
        public TaskCancellation cancelTask(UUID requestedTaskId) {
            throw new AssertionError("cancelTask must not be called");
        }

        @Override
        public void close() {}

        private static TaskCageConnectionException lostResponse(String stage) {
            return new TaskCageConnectionException(
                    "simulated " + stage + " response loss", new IOException("connection closed"));
        }

        private static FinishedRemoteProfileTaskSnapshot finished(
                UUID taskId, ManagedOutputArtifact output) {
            Instant started = Instant.parse("2026-08-22T00:00:01Z");
            ExecutionResult execution = new ExecutionResult(
                    TerminationReason.EXITED,
                    new ProcessResult(0, null),
                    new TaskTiming(
                            started.minusSeconds(1),
                            started,
                            started.plusSeconds(1),
                            Duration.ofSeconds(1)),
                    new TaskUsage(1, 1),
                    new TaskOutput("", "", false, false));
            return new FinishedRemoteProfileTaskSnapshot(
                    taskId,
                    new ProfileIdentity(CAPSULE.name(), CAPSULE.version()),
                    ProfileOutcome.SUCCEEDED,
                    execution,
                    Map.of(FILE_REQUEST.outputSlot(), output),
                    null);
        }
    }
}
