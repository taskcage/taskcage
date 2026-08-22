package org.taskcage.sdk;

import java.util.regex.Pattern;

final class IdentityNames {
    private static final Pattern CAPSULE = Pattern.compile(
            "[a-z][a-z0-9-]*(?:\\.[a-z][a-z0-9-]*)*");

    private IdentityNames() {}

    static boolean isValidCapsuleName(String value) {
        return value.length() <= 63 && CAPSULE.matcher(value).matches();
    }
}
