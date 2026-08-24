package org.taskcage.sdk;

import java.net.URI;
import java.security.GeneralSecurityException;
import java.security.SecureRandom;
import java.time.Duration;
import java.util.Objects;
import javax.net.ssl.SSLContext;
import javax.net.ssl.TrustManager;
import javax.net.ssl.X509TrustManager;
import java.security.cert.X509Certificate;

/** Immutable TLS connection configuration for the authenticated Remote Runtime. */
public record RemoteConnectionOptions(
        URI endpoint,
        ServiceCredentials credentials,
        SSLContext sslContext,
        TlsVerificationMode tlsVerification,
        Duration connectTimeout,
        Duration requestTimeout) {
    public RemoteConnectionOptions {
        endpoint = validateEndpoint(endpoint);
        credentials = Objects.requireNonNull(credentials, "credentials");
        sslContext = Objects.requireNonNull(sslContext, "sslContext");
        tlsVerification = Objects.requireNonNull(tlsVerification, "tlsVerification");
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
        private TlsVerificationMode tlsVerification = TlsVerificationMode.PREFERRED;
        private boolean customSslContext;
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
            this.customSslContext = true;
            return this;
        }

        /** Selects how the daemon certificate is verified. The default is {@link TlsVerificationMode#PREFERRED}. */
        public Builder tlsVerification(TlsVerificationMode tlsVerification) {
            this.tlsVerification = tlsVerification;
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
            if (tlsVerification == TlsVerificationMode.PREFERRED && !customSslContext) {
                sslContext = preferredSslContext();
            }
            return new RemoteConnectionOptions(
                    endpoint, credentials, sslContext, tlsVerification, connectTimeout, requestTimeout);
        }

        private static SSLContext preferredSslContext() {
            try {
                SSLContext context = SSLContext.getInstance("TLS");
                context.init(null, new TrustManager[] {LoopbackTrustManager.INSTANCE}, new SecureRandom());
                return context;
            } catch (GeneralSecurityException exception) {
                throw new IllegalStateException("could not configure preferred TLS", exception);
            }
        }
    }

    private enum LoopbackTrustManager implements X509TrustManager {
        INSTANCE;

        @Override
        public void checkClientTrusted(X509Certificate[] chain, String authType) {}

        @Override
        public void checkServerTrusted(X509Certificate[] chain, String authType) {}

        @Override
        public X509Certificate[] getAcceptedIssuers() {
            return new X509Certificate[0];
        }
    }
}
