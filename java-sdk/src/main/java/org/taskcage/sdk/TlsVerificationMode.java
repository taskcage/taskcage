package org.taskcage.sdk;

/** Server-certificate verification policy for a Remote TaskCage TLS connection. */
public enum TlsVerificationMode {
    /**
     * Require TLS encryption but accept the daemon certificate without CA or hostname verification.
     *
     * <p>This mirrors MySQL's convenient {@code PREFERRED} connection experience, except TaskCage never
     * falls back to plaintext. It is suitable only when the network path is already trusted.
     */
    PREFERRED,

    /** Verify the issuing CA and that the certificate matches the endpoint hostname. */
    VERIFY_IDENTITY,

    /** Verify the issuing CA but do not compare the certificate with the endpoint hostname. */
    VERIFY_CA,

}
