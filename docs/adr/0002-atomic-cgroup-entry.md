# ADR 0002: Atomic cgroup entry with clone3

- Status: Accepted
- Date: 2026-07-20

## Context

Starting a target and then writing its PID to `cgroup.procs` allows the target to run or create descendants before limits apply. PID snapshots and post-start moves cannot close this race reliably.

## Decision

The Rust daemon opens the prepared cgroup v2 directory and creates the target with `clone3(CLONE_INTO_CGROUP)`. The child begins life in the job cgroup and executes the target without a shell. Arguments, environment strings and file-descriptor actions are prepared in the parent before `clone3`; the child-side path is kept allocation-free and async-signal-safe until `execve`.

The capability is mandatory for the contest MVP. Unsupported kernels or permissions fail closed; there is no post-start attach fallback.

## Consequences

- The target never executes outside the resource-controlled job cgroup.
- The previous READY/GO attach barrier is unnecessary.
- Kernel, permission and syscall capability checks become part of preflight.
- Compatibility is narrower, which is acceptable for the pinned Ubuntu MVP target.
- A future compatibility fallback would require a separately reviewed design and must preserve the same guarantee.
