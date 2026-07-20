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

## Implementation guardrails

- Resolve and validate the delegated cgroup root before target creation.
- Prepare executable path, argv, environment and file-descriptor state in the
  parent before `clone3`.
- Use a close-on-exec error pipe so the parent can distinguish successful
  `execve` from child setup failure.
- Restrict the child-side path to async-signal-safe syscalls and terminate with
  `_exit` on failure.
- Set a parent-death signal and verify that the expected parent is still alive.
- Never add a post-start `cgroup.procs` move as an automatic fallback.

## Verification

- Unit-test argv preparation, wait-status decoding and cgroup parsers.
- Run `integration-tests/cgroup-smoke.sh` inside a transient systemd service
  with `Delegate=yes`.
- Use the bounded `ghost-tree` fixture to prove that descendants are killed
  after the leader exits.
- Require `cgroup.events` to report `populated 0` before directory removal.

## Remaining risks

- The post-`clone3` child path requires focused unsafe-code review.
- Kernel, systemd and delegated-controller availability vary by distribution.
- The first smoke slice inherits stdout and stderr; bounded capture is a later
  lifecycle milestone.
