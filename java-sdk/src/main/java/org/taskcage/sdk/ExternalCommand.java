package org.taskcage.sdk;

import java.nio.file.Path;
import java.util.List;
import java.util.Map;
import java.util.Objects;

/** An executable and arguments passed directly to the daemon without a shell. */
public record ExternalCommand(Path program, List<String> arguments, Path workingDirectory, Map<String, String> environment) {
    public ExternalCommand {
        program = absolute(program, "program");
        workingDirectory = absolute(workingDirectory, "workingDirectory");
        arguments = List.copyOf(Objects.requireNonNull(arguments, "arguments"));
        environment = Map.copyOf(Objects.requireNonNull(environment, "environment"));
        arguments.forEach(argument -> Objects.requireNonNull(argument, "arguments must not contain null"));
        environment.forEach((key, value) -> {
            Objects.requireNonNull(key, "environment key");
            Objects.requireNonNull(value, "environment value");
        });
    }

    private static Path absolute(Path path, String name) {
        Objects.requireNonNull(path, name);
        if (!path.isAbsolute()) {
            throw new IllegalArgumentException(name + " must be an absolute path");
        }
        return path;
    }
}
