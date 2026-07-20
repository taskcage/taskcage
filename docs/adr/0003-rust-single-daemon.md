# ADR 0003: Rust single daemon

- Status: Accepted
- Date: 2026-07-20
- Authority: repository `README.md`

## Context

The contest MVP needs a Linux control plane for cgroup lifecycle, scheduling,
monitoring, cleanup and a local protocol for the Java SDK. A prior proposal
split the daemon into Go and a small Rust launcher, which introduced two native
toolchains and an additional runtime contract.

## Decision

- Implement the Linux management program as one Rust `taskcaged` daemon.
- Keep the Java 21 SDK and Spring Boot starter as separate JVM modules.
- Communicate over a versioned, length-prefixed JSON protocol on a Unix Domain
  Socket.
- Create targets atomically in their job cgroup with
  `clone3(CLONE_INTO_CGROUP)`; do not add a post-start PID-move fallback.
- Keep target execution shell-free and keep the post-clone child path minimal.
- Treat the root `README.md` as the product decision source of truth.

## Consequences

- Native builds, releases and operational debugging use one Rust toolchain.
- The daemon owns all policy and kernel interaction in one codebase.
- Rust concurrency and direct process creation are on the MVP critical path.
- The first technical gate must prove atomic start, complete cgroup cleanup and
  repeatability before SDK convenience work expands.
- ADR 0001 is superseded. ADR 0002 remains accepted with Rust as its
  implementation language.
