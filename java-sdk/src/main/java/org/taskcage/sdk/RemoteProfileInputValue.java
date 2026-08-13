package org.taskcage.sdk;

/** A value accepted by the language-neutral Remote Profile contract. */
public sealed interface RemoteProfileInputValue
        permits RemoteStringInput, RemoteInt64Input, RemoteBooleanInput, ManagedInputArtifact {}
