# Linux integration tests

These tests require a real Linux cgroup v2 environment with a delegated subtree. They must verify normal exit, wall timeout, memory OOM, PID limit, output limit, cancellation, orphan cleanup, daemon restart recovery and bounded concurrency.

Container-only mock tests do not count as evidence for process-tree cleanup.
