package org.taskcage.sdk.internal.protocol.common;

import com.fasterxml.jackson.databind.JsonNode;
import java.time.Instant;
import java.util.ArrayList;
import java.util.List;

/** Shared strict field readers for TaskCage JSON protocols. */
public final class JsonFields {
    private JsonFields() {}

    public static String requiredText(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || !value.isTextual() || value.textValue().isEmpty()) {
            throw new IllegalArgumentException(field + " must be a non-empty string");
        }
        return value.textValue();
    }

    public static String requiredNonBlankText(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || !value.isTextual() || value.textValue().isBlank()) {
            throw new IllegalArgumentException(field + " must be a non-blank string");
        }
        return value.textValue();
    }

    public static String requiredString(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || !value.isTextual()) {
            throw new IllegalArgumentException(field + " must be a string");
        }
        return value.textValue();
    }

    public static JsonNode requiredObject(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || !value.isObject()) {
            throw new IllegalArgumentException(field + " must be an object");
        }
        return value;
    }

    public static boolean requiredBoolean(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || !value.isBoolean()) {
            throw new IllegalArgumentException(field + " must be a boolean");
        }
        return value.booleanValue();
    }

    public static int requiredPositiveInt(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || !value.isIntegralNumber() || !value.canConvertToInt() || value.intValue() <= 0) {
            throw new IllegalArgumentException(field + " must be a positive integer");
        }
        return value.intValue();
    }

    public static long requiredPositiveLong(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || !value.isIntegralNumber() || !value.canConvertToLong() || value.longValue() <= 0) {
            throw new IllegalArgumentException(field + " must be a positive integer");
        }
        return value.longValue();
    }

    public static long requiredNonNegativeLong(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || !value.isIntegralNumber() || !value.canConvertToLong() || value.longValue() < 0) {
            throw new IllegalArgumentException(field + " must be a non-negative integer");
        }
        return value.longValue();
    }

    public static int requiredInt(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || !value.isIntegralNumber() || !value.canConvertToInt()) {
            throw new IllegalArgumentException(field + " must be an integer");
        }
        return value.intValue();
    }

    /** Local Protocol nullable integer reader; preserves its historical error text. */
    public static Integer optionalInt(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || value.isNull()) {
            return null;
        }
        return requiredInt(object, field);
    }

    /** Remote Protocol nullable integer reader; preserves its historical error text. */
    public static Integer nullableInt(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || value.isNull()) {
            return null;
        }
        if (!value.isIntegralNumber() || !value.canConvertToInt()) {
            throw new IllegalArgumentException(field + " must be an integer or null");
        }
        return value.intValue();
    }

    /** Local Protocol nullable text reader; preserves its historical error text. */
    public static String optionalText(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || value.isNull()) {
            return null;
        }
        return requiredText(object, field);
    }

    /** Remote Protocol nullable text reader; preserves its historical error text. */
    public static String nullableText(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || value.isNull()) {
            return null;
        }
        if (!value.isTextual() || value.textValue().isEmpty()) {
            throw new IllegalArgumentException(field + " must be a non-empty string or null");
        }
        return value.textValue();
    }

    public static Instant requiredInstant(JsonNode object, String field) {
        return Instant.parse(requiredText(object, field));
    }

    /** Local Protocol enum reader; preserves its historical error text. */
    public static <T extends Enum<T>> T requiredEnum(JsonNode object, String field, Class<T> type) {
        try {
            return Enum.valueOf(type, requiredText(object, field));
        } catch (IllegalArgumentException exception) {
            throw new IllegalArgumentException(field + " must be a supported " + type.getSimpleName(), exception);
        }
    }

    /** Remote Protocol enum reader; preserves its historical error text. */
    public static <T extends Enum<T>> T enumValue(JsonNode object, String field, Class<T> type) {
        try {
            return Enum.valueOf(type, requiredText(object, field));
        } catch (IllegalArgumentException exception) {
            throw new IllegalArgumentException(field + " is invalid", exception);
        }
    }

    public static List<Integer> requiredIntegerList(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || !value.isArray()) {
            throw new IllegalArgumentException(field + " must be an array");
        }
        List<Integer> values = new ArrayList<>();
        for (JsonNode entry : value) {
            if (!entry.isIntegralNumber() || !entry.canConvertToInt()) {
                throw new IllegalArgumentException(field + " must contain integers");
            }
            values.add(entry.intValue());
        }
        return values;
    }

    public static List<String> requiredTextList(JsonNode object, String field) {
        JsonNode value = object.get(field);
        if (value == null || !value.isArray()) {
            throw new IllegalArgumentException(field + " must be an array");
        }
        List<String> values = new ArrayList<>();
        for (JsonNode entry : value) {
            if (!entry.isTextual() || entry.textValue().isEmpty()) {
                throw new IllegalArgumentException(field + " must contain non-empty strings");
            }
            values.add(entry.textValue());
        }
        return values;
    }
}
