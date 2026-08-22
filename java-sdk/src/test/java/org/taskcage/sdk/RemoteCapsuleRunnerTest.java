package org.taskcage.sdk;

import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.io.IOException;
import java.nio.file.Path;
import java.time.Duration;
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
}
