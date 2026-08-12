package org.taskcage.sdk;

/** A value accepted by the language-neutral Local Profile Core API. */
public sealed interface ProfileInputValue
        permits StringProfileInput, Int64ProfileInput, BooleanProfileInput, LocalInputArtifact {}
