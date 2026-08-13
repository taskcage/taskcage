package org.taskcage.sdk.internal.remote;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.UUID;
import java.util.Map;
import org.junit.jupiter.api.Test;
import org.taskcage.sdk.ManagedInputArtifact;
import org.taskcage.sdk.ManagedOutputArtifact;
import org.taskcage.sdk.FinishedRemoteProfileTaskSnapshot;
import org.taskcage.sdk.RemoteArtifactUploadState;
import org.taskcage.sdk.ProfileIdentity;
import org.taskcage.sdk.ProfileResourceOverrides;
import org.taskcage.sdk.RemoteCapabilities;
import org.taskcage.sdk.RemoteProfileRequest;
import org.taskcage.sdk.RemoteProfileTask;
import org.taskcage.sdk.RunningRemoteProfileTaskSnapshot;
import org.taskcage.sdk.Secret;
import org.taskcage.sdk.ServiceCredentials;
import org.taskcage.sdk.Sha256Digest;
import org.taskcage.sdk.TaskCageDaemonException;
import org.taskcage.sdk.TaskCageProtocolException;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;

class RemoteProtocolCodecTest {
    private static final ObjectMapper MAPPER = new ObjectMapper();
    private static final UUID REQUEST_ID = UUID.fromString("11111111-1111-4111-8111-111111111111");

    @Test
    void authenticationRequestMatchesTheSharedFixture() throws Exception {
        RemoteProtocolCodec codec = new RemoteProtocolCodec();
        JsonNode actual = MAPPER.readTree(codec.authenticate(
                REQUEST_ID, ServiceCredentials.of("document-worker", Secret.of("fixture-secret-only"))));

        assertEquals(fixture("authenticate-request.json"), actual);
    }

    @Test
    void authenticationAndSecurityErrorsUseTheSharedResponseContract() throws Exception {
        RemoteProtocolCodec codec = new RemoteProtocolCodec();
        JsonNode authenticated = codec.readAndValidate(
                MAPPER.writeValueAsBytes(fixture("authenticated.json")), REQUEST_ID);
        codec.requireAuthenticated(authenticated);

        JsonNode error = codec.readAndValidate(
                MAPPER.writeValueAsBytes(fixture("error-authentication-failed.json")), REQUEST_ID);
        TaskCageDaemonException exception = assertThrows(
                TaskCageDaemonException.class, () -> { throw codec.decodeError(error); });
        assertEquals("AUTHENTICATION_FAILED", exception.code());
        assertEquals(false, exception.retryable());
    }

    @Test
    void rejectsLocalProtocolAndMalformedRemoteEnvelopes() throws Exception {
        RemoteProtocolCodec codec = new RemoteProtocolCodec();
        assertThrows(TaskCageProtocolException.class, () -> codec.readAndValidate(
                "{\"protocolVersion\":1,\"requestId\":\"11111111-1111-4111-8111-111111111111\",\"type\":\"authenticated\",\"payload\":{}}"
                        .getBytes(java.nio.charset.StandardCharsets.UTF_8),
                REQUEST_ID));
    }

    @Test
    void managedTransferAndProfileFramesMatchTheSharedFixtures() throws Exception {
        RemoteProtocolCodec codec = new RemoteProtocolCodec();
        UUID uploadRequest = UUID.fromString("33333333-3333-4333-8333-333333333333");
        UUID clientArtifactId = UUID.fromString("44444444-4444-4444-8444-444444444444");
        UUID artifactId = UUID.fromString("55555555-5555-4555-8555-555555555555");
        Sha256Digest inputDigest = new Sha256Digest(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

        assertEquals(fixture("get-capabilities.json"), MAPPER.readTree(codec.getCapabilities(
                UUID.fromString("22222222-2222-4222-8222-222222222222"))));
        assertEquals(fixture("begin-artifact-upload.json"), MAPPER.readTree(codec.beginArtifactUpload(
                uploadRequest, clientArtifactId, inputDigest, 1234, "audio/mpeg")));
        assertEquals(fixture("upload-artifact-chunk.json"), MAPPER.readTree(codec.uploadArtifactChunk(
                UUID.fromString("66666666-6666-4666-8666-666666666666"), artifactId, 0, "test".getBytes())));
        assertEquals(fixture("complete-artifact-upload.json"), MAPPER.readTree(codec.completeArtifactUpload(
                UUID.fromString("77777777-7777-4777-8777-777777777777"), artifactId)));
        assertEquals(MAPPER.readTree("""
                {"remoteProtocolVersion":1,"requestId":"88888888-8888-4888-8888-888888888888",
                 "type":"abortArtifactUpload","payload":{"artifactId":"55555555-5555-4555-8555-555555555555"}}
                """), MAPPER.readTree(codec.abortArtifactUpload(
                UUID.fromString("88888888-8888-4888-8888-888888888888"), artifactId)));
        assertEquals(fixture("read-artifact-chunk.json"), MAPPER.readTree(codec.readArtifactChunk(
                UUID.fromString("99999999-9999-4999-8999-999999999999"),
                UUID.fromString("88888888-8888-4888-8888-888888888888"), 0, 780000)));

        RemoteProfileRequest request = new RemoteProfileRequest(
                new ProfileIdentity("ffmpeg-audio-to-wav", "1.0.0"),
                Map.of("source", new ManagedInputArtifact(artifactId)),
                ProfileResourceOverrides.builder().wallTimeLimit(java.time.Duration.ofMinutes(5)).build());
        assertEquals(fixture("submit-profile-valid.json"), MAPPER.readTree(codec.submitProfile(
                uploadRequest, clientArtifactId, request)));
    }

    @Test
    void remoteCapabilitiesAndManagedArtifactsDecodeTheSharedFixtures() throws Exception {
        RemoteProtocolCodec codec = new RemoteProtocolCodec();
        RemoteCapabilities capabilities = codec.decodeCapabilities(fixture("capabilities.json"));
        assertEquals(true, capabilities.supportsManagedTransfer());
        assertEquals(780000, capabilities.maxArtifactChunkBytes());
        assertEquals(artifactId(), codec.decodeArtifactUploaded(fixture("artifact-uploaded.json")).artifactId());
        assertEquals(RemoteArtifactUploadState.UPLOADING, codec.decodeArtifactUploadStarted(MAPPER.readTree("""
                {"remoteProtocolVersion":1,"requestId":"33333333-3333-4333-8333-333333333333",
                 "type":"artifactUploadStarted","payload":{"artifactId":"55555555-5555-4555-8555-555555555555",
                 "state":"UPLOADING","nextOffset":0}}
                """)).state());
        assertEquals(4, codec.decodeArtifactChunkAccepted(fixture("artifact-chunk-accepted.json")).nextOffset());

        ManagedOutputArtifact output = codec.decodeManagedOutputArtifact(
                fixture("profile-result-success.json").path("payload").path("artifacts").path("result"));
        assertEquals("audio/wav", output.mediaType());
        assertEquals(UUID.fromString("88888888-8888-4888-8888-888888888888"), output.artifactId());
    }

    @Test
    void profileAcceptanceAndAllResultStatesDecodeTheSharedFixtures() throws Exception {
        RemoteProtocolCodec codec = new RemoteProtocolCodec();
        RemoteProfileTask accepted = codec.decodeProfileAccepted(fixture("profile-accepted.json"));
        assertEquals(artifactId(), accepted.taskId());
        assertEquals(java.time.Duration.ofMinutes(5), accepted.effectiveResources().wallTimeLimit());

        assertEquals(RunningRemoteProfileTaskSnapshot.class,
                codec.decodeProfileResult(fixture("profile-result-running.json")).getClass());
        FinishedRemoteProfileTaskSnapshot success = (FinishedRemoteProfileTaskSnapshot)
                codec.decodeProfileResult(fixture("profile-result-success.json"));
        assertEquals("audio/wav", success.artifacts().get("result").mediaType());
        FinishedRemoteProfileTaskSnapshot failure = (FinishedRemoteProfileTaskSnapshot)
                codec.decodeProfileResult(fixture("profile-result-failed.json"));
        assertEquals("PROCESS_EXITED_NONZERO", failure.failure().code());
    }

    private static UUID artifactId() {
        return UUID.fromString("55555555-5555-4555-8555-555555555555");
    }

    private static JsonNode fixture(String name) throws Exception {
        String root = System.getProperty("taskcage.protocolFixturesDir");
        return MAPPER.readTree(Files.readString(Path.of(root).resolve("remote-v1").resolve(name)));
    }
}
