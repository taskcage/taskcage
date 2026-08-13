package org.taskcage.sdk;

import java.net.URI;
import java.time.Duration;
import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;

class RemoteConnectionOptionsTest {
    private static final ServiceCredentials CREDENTIALS =
            ServiceCredentials.of("document-worker", Secret.of("fixture-secret"));

    @Test
    void acceptsOnlyTheSecretFreeTlsEndpointForm() {
        RemoteConnectionOptions options = RemoteConnectionOptions.builder(
                        URI.create("taskcage+tls://taskcage.internal:7443"), CREDENTIALS)
                .connectTimeout(Duration.ofSeconds(2))
                .requestTimeout(Duration.ofSeconds(10))
                .build();

        assertEquals("taskcage+tls://taskcage.internal:7443", options.endpoint().toString());
        assertEquals(Duration.ofSeconds(2), options.connectTimeout());
        assertEquals(Duration.ofSeconds(10), options.requestTimeout());
        assertFalse(options.credentials().toString().contains("fixture-secret"));
    }

    @Test
    void rejectsEndpointFormsThatCouldLeakSecretsOrSkipTlsIdentity() {
        assertThrows(IllegalArgumentException.class, () -> RemoteConnectionOptions.builder(
                URI.create("taskcage://taskcage.internal:7443"), CREDENTIALS).build());
        assertThrows(IllegalArgumentException.class, () -> RemoteConnectionOptions.builder(
                URI.create("taskcage+tls://worker:secret@taskcage.internal:7443"), CREDENTIALS).build());
        assertThrows(IllegalArgumentException.class, () -> RemoteConnectionOptions.builder(
                URI.create("taskcage+tls://taskcage.internal:7443/?token=secret"), CREDENTIALS).build());
        assertThrows(IllegalArgumentException.class, () -> RemoteConnectionOptions.builder(
                URI.create("taskcage+tls://taskcage.internal"), CREDENTIALS).build());
    }

    @Test
    void validatesServiceAccountWireSyntaxAndTimeouts() {
        assertThrows(IllegalArgumentException.class,
                () -> ServiceCredentials.of("DocumentWorker", Secret.of("fixture-secret")));
        assertThrows(IllegalArgumentException.class, () -> RemoteConnectionOptions.builder(
                        URI.create("taskcage+tls://taskcage.internal:7443"), CREDENTIALS)
                .connectTimeout(Duration.ZERO)
                .build());
    }
}
