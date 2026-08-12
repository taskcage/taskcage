package org.taskcage.example.ffmpeg;

import java.io.ByteArrayOutputStream;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;

/** Creates the deterministic input used only by the containerized example. */
public final class GenerateInput {
    private GenerateInput() {}

    public static void main(String[] args) throws Exception {
        if (args.length != 1) {
            throw new IllegalArgumentException("usage: GenerateInput <output-path>");
        }
        Path output = Path.of(args[0]);
        Files.createDirectories(output.getParent());
        Files.write(output, sineWave());
    }

    private static byte[] sineWave() {
        int sampleRate = 44_100;
        int sampleCount = sampleRate / 4;
        ByteArrayOutputStream output = new ByteArrayOutputStream(44 + sampleCount * 2);
        output.writeBytes("RIFF".getBytes(StandardCharsets.US_ASCII));
        writeInt(output, 36 + sampleCount * 2);
        output.writeBytes("WAVEfmt ".getBytes(StandardCharsets.US_ASCII));
        writeInt(output, 16);
        writeShort(output, 1);
        writeShort(output, 1);
        writeInt(output, sampleRate);
        writeInt(output, sampleRate * 2);
        writeShort(output, 2);
        writeShort(output, 16);
        output.writeBytes("data".getBytes(StandardCharsets.US_ASCII));
        writeInt(output, sampleCount * 2);
        for (int index = 0; index < sampleCount; index++) {
            double phase = 2.0 * Math.PI * 440.0 * index / sampleRate;
            writeShort(output, (int) (Math.sin(phase) * 8_000));
        }
        return output.toByteArray();
    }

    private static void writeInt(ByteArrayOutputStream output, int value) {
        output.writeBytes(ByteBuffer.allocate(4)
                .order(ByteOrder.LITTLE_ENDIAN)
                .putInt(value)
                .array());
    }

    private static void writeShort(ByteArrayOutputStream output, int value) {
        output.writeBytes(ByteBuffer.allocate(2)
                .order(ByteOrder.LITTLE_ENDIAN)
                .putShort((short) value)
                .array());
    }
}
