package org.taskcage.sdk;

import java.util.Objects;
import java.util.regex.Pattern;

/** A non-interactive service account used by the Remote Runtime. */
public record ServiceCredentials(String clientId, Secret secret) {
    private static final Pattern CLIENT_ID = Pattern.compile("[a-z][a-z0-9-]{0,62}");

    public ServiceCredentials {
        Objects.requireNonNull(clientId, "clientId");
        Objects.requireNonNull(secret, "secret");
        if (!CLIENT_ID.matcher(clientId).matches()) {
            throw new IllegalArgumentException("clientId must match [a-z][a-z0-9-]{0,62}");
        }
    }

    public static ServiceCredentials of(String clientId, Secret secret) {
        return new ServiceCredentials(clientId, secret);
    }

    @Override
    public String toString() {
        return "ServiceCredentials[clientId=" + clientId + ", secret=[secret]]";
    }
}
