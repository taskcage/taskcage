package org.taskcage.sdk.internal.remote;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.UUID;
import org.junit.jupiter.api.Test;
import org.taskcage.sdk.Secret;
import org.taskcage.sdk.ServiceCredentials;
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

    private static JsonNode fixture(String name) throws Exception {
        String root = System.getProperty("taskcage.protocolFixturesDir");
        return MAPPER.readTree(Files.readString(Path.of(root).resolve("remote-v1").resolve(name)));
    }
}
