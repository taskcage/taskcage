package org.taskcage.sdk;

/** The daemon's direct response after attempting to submit a Profile Task. */
public sealed interface ProfileTaskSubmission permits ProfileTask, FinishedProfileTaskSnapshot {}
