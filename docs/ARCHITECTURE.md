# TaskCage MVP Architecture

> Authority: the repository root [`README.md`](../README.md). This document
> elaborates the final Rust-daemon decision and may not override it.

## 1. Decision summary

TaskCage has one native Linux management program and a Java integration layer.

| Component | Technology | Responsibility |
|---|---|---|
| `taskcaged` | Rust, systemd | UDS, admission control, cgroup lifecycle, atomic process creation, monitoring, classification and recovery |
| TaskCage API | Java 21 | command, resource budget and execution-result types |
| TaskCage client | Java 21 | versioned UDS protocol transport |
| Spring Boot starter | Java 21 | auto-configuration, properties and minimal metrics |

There is no Go daemon and no separate launcher in the final MVP structure.
The Rust daemon creates the target directly inside its prepared cgroup.

## 2. System view

```text
Spring application
    |
    | taskCage.execute(command, budget)
    v
Java SDK
    |
    | versioned length-prefixed JSON over Unix Domain Socket
    v
taskcaged (Rust, systemd service)
    |
    +--> Preflight and peer authorization
    +--> Bounded FIFO admission queue
    +--> Delegated cgroup v2 manager
    +--> clone3(CLONE_INTO_CGROUP) + execve target
    +--> Output, time and kernel-event monitor
    `--> Classifier and cleanup --> ExecutionResult
```

## 3. Delegated cgroup layout

The MVP uses one systemd service account and one delegated subtree.

```text
taskcaged.service                  delegated root
├── manager                       daemon moves itself here
└── jobs                          controller-owning internal node
    ├── job-<id>                  target process tree leaf
    └── job-<id>
```

Startup order:

1. systemd starts `taskcaged` with `Delegate=yes`.
2. The daemon resolves its own cgroup from `/proc/self/cgroup`.
3. It creates `manager`, moves itself there and verifies membership.
4. It creates `jobs` and enables the required controllers in the correct
   parent `cgroup.subtree_control`.
5. It checks cgroup v2, `cpu`, `memory`, `pids`, `cgroup.kill`, permissions and
   atomic process-entry support.
6. It kills and removes stale `job-*` groups before accepting requests.

TaskCage never writes outside its canonical delegated root and remains the
single writer for that subtree.

## 4. Rust daemon modules

| Module | Scope |
|---|---|
| `protocol` | frame codec, request validation, response encoding and version rules |
| `scheduler` | maximum active jobs, bounded FIFO queue, queue timeout and permit ownership |
| `cgroup` | root discovery, controller setup, job leaves, limits, evidence and removal |
| `executor` | pipe preparation, `clone3`, pidfd where supported and shell-free `execve` |
| `monitor` | stdout/stderr drain, monotonic timers, event snapshots and termination trigger |
| `classifier` | deterministic status and termination-reason priority |
| `recovery` | startup scavenging and shutdown cleanup |

Policy stays in the parent daemon. The child path between `clone3` and
`execve` must not parse configuration, allocate memory, acquire locks or emit
structured logs.

## 5. Atomic target creation

Starting a process and later moving its PID into `cgroup.procs` creates a race.
The final MVP instead uses the following sequence:

1. Create an empty `jobs/job-<id>` leaf.
2. Apply and read back supported limits.
3. Capture baseline kernel-event counters.
4. Open the job cgroup directory and prepare argv, environment, working
   directory, pipes and the child error channel.
5. Call `clone3` with `CLONE_INTO_CGROUP` and request a pidfd where supported.
6. In the child, set parent-death behavior, install prepared file descriptors,
   change directory and call shell-free `execve`.
7. In the parent, close child-only descriptors and begin monitoring.

The target and all descendants begin inside the job cgroup. Unsupported
kernels or permissions return `UNSUPPORTED`; there is no post-start move
fallback.

## 6. Job lifecycle

```text
RECEIVED
   |
   v
QUEUED -- capacity/queue timeout --> REJECTED
   |
   v
PREPARING -- preflight/start error --> UNSUPPORTED or INTERNAL_ERROR
   |
   v
RUNNING -- timeout/limit/cancel --> KILLING
   |                                  |
   | target exit                      v
   +----------------------------> COLLECTING
                                      |
                                      v
                                   CLEANUP
                                      |
                                      v
                                    RESULT
```

One lifecycle object owns the execution permit. It releases the permit exactly
once, only after the job cgroup is empty or cleanup failure is explicitly
recorded.

## 7. Limits and evidence

| Budget | Mechanism | Evidence |
|---|---|---|
| CPU quota | `cpu.max` | `cpu.stat` |
| memory | `memory.max` | `memory.events.local`, `memory.peak` |
| process count | `pids.max` | `pids.events`, `pids.peak` when available |
| wall time | monotonic Rust timer | watchdog state |
| output size | simultaneous bounded stream drains | byte counters and truncation state |
| concurrency | daemon scheduler | active and queued counters |

Event files are read before execution and after termination. Classification
uses counter deltas rather than exit-code guesses.

Initial reason priority:

1. caller cancellation
2. queue capacity or queue timeout
3. wall timeout
4. output limit
5. memory OOM event delta
6. PID-limit event delta
7. normal or non-zero exit
8. signal or unknown failure

## 8. Termination and cleanup

For cancellation, timeout or a policy violation, the daemon:

1. records the first terminal trigger;
2. writes `1` to the job's `cgroup.kill`;
3. waits within a bounded cleanup timeout for `cgroup.events` to report
   `populated 0`;
4. collects final counters and usage;
5. removes the empty cgroup;
6. returns the result and releases the scheduler permit.

Killing only the leader PID, process group or a point-in-time descendant list
is not successful TaskCage cleanup.

## 9. Crash recovery

- systemd restarts `taskcaged` after failure.
- The target child receives a parent-death signal as defense in depth.
- Startup scavenging treats every stale populated `job-*` as unowned, invokes
  `cgroup.kill`, waits for emptiness and removes it.
- Cross-restart result replay is outside protocol v1.

Cleanup is proven by cgroup state, not by the parent-death signal alone.

## 10. Protocol and trust model

- Socket: `/run/taskcage/taskcaged.sock`
- Framing: four-byte big-endian length followed by UTF-8 JSON
- One connection per synchronous job in protocol v1
- Commands are argv arrays; shell command strings are rejected
- Socket permissions and Linux peer credentials restrict the caller
- Frame size, budget ceilings and working-directory policy are daemon-owned
- Closing an active connection is interpreted as caller cancellation

See [`PROTOCOL.md`](PROTOCOL.md) for the wire contract.

## 11. Security boundary

TaskCage controls resource use and process-tree cleanup. It does not isolate
filesystem access, network access, system calls or kernel attack surface. The
MVP must not be described as a security sandbox.

Namespaces, seccomp, AppArmor, SELinux and untrusted-code execution are outside
the final MVP scope.

## 12. Compatibility target

The contest release pins one Ubuntu LTS x86-64 environment first. Runtime
preflight checks capabilities instead of trusting version strings:

- unified cgroup v2
- delegated writable subtree
- `cpu`, `memory` and `pids` controllers
- `clone3(CLONE_INTO_CGROUP)`
- `cgroup.kill`
- required statistics and event files

Ubuntu 22.04/24.04 expansion and ARM64 are follow-up compatibility work after
the pinned environment passes all gates.

## 13. Non-negotiable invariants

- A target never runs outside its configured job cgroup.
- Unsupported protection fails closed before target execution.
- Every command is executed without a shell.
- Every terminal path attempts whole-cgroup cleanup.
- Cleanup success requires an empty cgroup.
- Kernel evidence takes precedence over ambiguous exit codes.
- Queue permits are returned exactly once after cleanup.
- The root README wins over conflicting design documents.
