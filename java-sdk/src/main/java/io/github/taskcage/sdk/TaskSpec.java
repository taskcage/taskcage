package io.github.taskcage.sdk;

import java.util.Objects;

/** Immutable request to run one constrained external command. */
public record TaskSpec(ExternalCommand command, ResourceBudget budget) {
    public TaskSpec {
        Objects.requireNonNull(command, "command");
        Objects.requireNonNull(budget, "budget");
    }
}
