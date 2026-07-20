# ADR 0001: Go daemon and Rust launcher

- Status: Superseded by ADR 0003
- Date: 2026-07-20

## Context

TaskCage needs a long-lived Linux control plane for cgroup lifecycle, scheduling, monitoring and cleanup, plus a minimal and auditable boundary immediately before target execution. The Java SDK should not require direct cgroup permissions.

## Decision

- Implement `taskcaged` in Go.
- Implement `taskcage-launcher` in Rust.
- Connect the Java 21 SDK to the daemon over a versioned Unix Domain Socket protocol.
- Keep all cgroup policy and termination classification in Go.
- Keep the Rust launcher limited to process hardening and shell-free `exec`.

## Consequences

- Go is well suited to concurrent job monitoring, queues, UDS and a static daemon binary.
- Rust provides a small memory-safe launcher without a garbage-collected runtime.
- Three language toolchains increase CI and release work.
- The strict component boundary prevents duplicated policy and keeps the Rust portion reviewable.
- The daemon becomes an MVP requirement rather than a post-MVP agent.

## Supersession

The repository README was adopted as the final product decision on 2026-07-20.
It selects a single Rust `taskcaged` daemon, so this split-language proposal is
retained only as decision history.
