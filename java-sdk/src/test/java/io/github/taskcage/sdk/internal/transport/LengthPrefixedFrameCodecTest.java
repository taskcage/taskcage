package io.github.taskcage.sdk.internal.transport;

import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;

class LengthPrefixedFrameCodecTest {
    @Test
    void selectorTimeoutRoundsUpWithoutOverflow() {
        assertEquals(1, LengthPrefixedFrameCodec.timeoutMillisCeiling(1));
        assertEquals(1, LengthPrefixedFrameCodec.timeoutMillisCeiling(999_999));
        assertEquals(1, LengthPrefixedFrameCodec.timeoutMillisCeiling(1_000_000));
        assertEquals(2, LengthPrefixedFrameCodec.timeoutMillisCeiling(1_000_001));
        assertEquals(9_223_372_036_855L, LengthPrefixedFrameCodec.timeoutMillisCeiling(Long.MAX_VALUE));
    }
}
