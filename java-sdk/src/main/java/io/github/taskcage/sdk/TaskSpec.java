package io.github.taskcage.sdk;

import java.util.Objects;

/** Immutable request to run one constrained external command. */
public record TaskSpec(ExternalCommand command, ResourceBudget budget) {
    /** Creates a task request with {@link ResourceBudget#safeDefaults()}. */
    public TaskSpec(ExternalCommand command) {
        this(command, ResourceBudget.safeDefaults());
    }

    public TaskSpec {
        Objects.requireNonNull(command, "command");
        Objects.requireNonNull(budget, "budget");
    }
}
