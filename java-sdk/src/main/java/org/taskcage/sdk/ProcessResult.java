package org.taskcage.sdk;

/** Exit information reported by the daemon. Both fields are absent when exec never started. */
public record ProcessResult(Integer exitCode, String signal) {}
