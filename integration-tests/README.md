# Linux integration tests

These tests require a real Linux cgroup v2 environment and a systemd service
or scope with `Delegate=yes`. Container-only mocks do not count as evidence for
process-tree cleanup.

## First cgroup smoke test

The first vertical slice proves that TaskCage can:

1. discover its delegated cgroup v2 root;
2. move the daemon into a manager leaf;
3. enable `cpu`, `memory` and `pids` controllers;
4. create a limited job cgroup;
5. start `ghost-tree` with `clone3(CLONE_INTO_CGROUP)`;
6. observe the target PID in the job's `cgroup.procs`;
7. kill descendants after the fixture leader exits;
8. observe `populated 0` and remove the job cgroup.

The same script also runs a bounded wall-time scenario with `sleep`, requires a
timeout result and verifies complete cleanup.

Run on a disposable Ubuntu test VM:

```bash
./integration-tests/cgroup-smoke.sh
```

The script exits with status 77 when the host is not Linux, systemd is not
available or passwordless privilege is unavailable. It never creates cgroups
outside a temporary systemd unit delegated specifically to the smoke test.

Future scenarios add memory OOM, PID limit, output limit, cancellation, daemon
restart recovery and bounded concurrency.
