# ADR 0002: Atomic cgroup entry with clone3

- Status: Accepted
- Date: 2026-07-20

## Context

Starting a target and then writing its PID to `cgroup.procs` allows the target to run or create descendants before limits apply. PID snapshots and post-start moves cannot close this race reliably.

## Decision

The Go daemon opens the prepared cgroup v2 directory and starts the Rust launcher with `syscall.SysProcAttr.UseCgroupFD` and `CgroupFD`. Go uses `clone3(CLONE_INTO_CGROUP)` so the launcher begins life in the job cgroup and then `exec`s the target.

The capability is mandatory for the contest MVP. Unsupported kernels or permissions fail closed; there is no post-start attach fallback.

## Consequences

- The target never executes outside the resource-controlled job cgroup.
- The previous READY/GO attach barrier is unnecessary.
- Kernel and Go runtime capability checks become part of preflight.
- Compatibility is narrower, which is acceptable for the pinned Ubuntu MVP target.
- A future compatibility fallback would require a separately reviewed design and must preserve the same guarantee.
