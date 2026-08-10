package org.taskcage.sdk;

/** The daemon's direct response after attempting to submit a task. */
public sealed interface TaskSubmission permits Task, FinishedTaskSnapshot {}
