package org.taskcage.sdk;

import java.net.URI;
import java.time.Duration;
import java.util.Objects;
import javax.net.ssl.SSLContext;

/** Immutable TLS connection configuration for the authenticated Remote Runtime. */
public record RemoteConnectionOptions(
        URI endpoint,
        ServiceCredentials credentials,
        SSLContext sslContext,
        Duration connectTimeout,
        Duration requestTimeout) {
    public RemoteConnectionOptions {
        endpoint = validateEndpoint(endpoint);
        credentials = Objects.requireNonNull(credentials, "credentials");
        sslContext = Objects.requireNonNull(sslContext, "sslContext");
        connectTimeout = requirePositive(connectTimeout, "connectTimeout");
        requestTimeout = requirePositive(requestTimeout, "requestTimeout");
    }

    public static Builder builder(URI endpoint, ServiceCredentials credentials) {
        return new Builder(endpoint, credentials);
    }

    private static URI validateEndpoint(URI endpoint) {
        Objects.requireNonNull(endpoint, "endpoint");
        if (!"taskcage+tls".equals(endpoint.getScheme())
                || endpoint.getHost() == null
                || endpoint.getHost().isBlank()
                || endpoint.getPort() < 1
                || endpoint.getPort() > 65_535
                || endpoint.getUserInfo() != null
                || endpoint.getQuery() != null
                || endpoint.getFragment() != null
                || (endpoint.getPath() != null && !endpoint.getPath().isEmpty() && !"/".equals(endpoint.getPath()))) {
            throw new IllegalArgumentException(
                    "endpoint must be taskcage+tls://host:port without user info, query, fragment, or path");
        }
        return endpoint.normalize();
    }

    private static Duration requirePositive(Duration value, String name) {
        Objects.requireNonNull(value, name);
        try {
            if (value.isZero() || value.isNegative() || value.toNanos() <= 0) {
                throw new IllegalArgumentException(name + " must be positive and representable in nanoseconds");
            }
            return value;
        } catch (ArithmeticException exception) {
            throw new IllegalArgumentException(name + " must be representable in nanoseconds", exception);
        }
    }

    public static final class Builder {
        private final URI endpoint;
        private final ServiceCredentials credentials;
        private SSLContext sslContext;
        private Duration connectTimeout = Duration.ofSeconds(3);
        private Duration requestTimeout = Duration.ofSeconds(30);

        private Builder(URI endpoint, ServiceCredentials credentials) {
            this.endpoint = endpoint;
            this.credentials = credentials;
            try {
                this.sslContext = SSLContext.getDefault();
            } catch (Exception exception) {
                throw new IllegalStateException("could not load the platform TLS configuration", exception);
            }
        }

        public Builder sslContext(SSLContext sslContext) {
            this.sslContext = sslContext;
            return this;
        }

        public Builder connectTimeout(Duration connectTimeout) {
            this.connectTimeout = connectTimeout;
            return this;
        }

        public Builder requestTimeout(Duration requestTimeout) {
            this.requestTimeout = requestTimeout;
            return this;
        }

        public RemoteConnectionOptions build() {
            return new RemoteConnectionOptions(endpoint, credentials, sslContext, connectTimeout, requestTimeout);
        }
    }
}
