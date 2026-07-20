# TaskCage MVP Architecture

## 1. Decision summary

TaskCage is a local Linux job-management program and Java SDK composed of three language boundaries.

| Component | Language | Responsibility |
|---|---|---|
| `taskcaged` | Go | systemd delegation, UDS server, admission control, cgroup lifecycle, monitoring, classification, recovery |
| `taskcage-launcher` | Rust | minimal pre-exec boundary, parent-death signal, shell-free target `exec` |
| TaskCage SDK | Java 21 | public API, local protocol client, Spring Boot integration, Micrometer bridge |

Go owns all policy and cgroup semantics. Rust must remain intentionally small and must not duplicate scheduling, monitoring or classification logic. Java does not write to cgroupfs in the daemon-backed MVP.

## 2. System view

```text
Spring application
    |
    | TaskCage.run(command, budget)
    v
Java SDK
    |
    | versioned length-prefixed JSON over Unix Domain Socket
    v
taskcaged (Go, outside all job cgroups)
    |
    +--> Admission Controller
    |       |- global concurrency limit
    |       |- bounded FIFO queue
    |       `- queue timeout
    |
    +--> Cgroup Manager
    |       |- create job leaf
    |       |- apply memory/cpu/pids limits
    |       |- read kernel evidence
    |       `- kill, verify empty, delete
    |
    +--> Executor
    |       `- clone3(CLONE_INTO_CGROUP)
    |                  |
    |                  v
    |          taskcage-launcher (Rust)
    |                  `- exec target argv
    |
    +--> Output and Event Monitor
    |
    `--> Classifier --> ExecutionResult --> Java SDK
```

## 3. Delegated cgroup layout

The MVP uses one systemd service account and one delegated cgroup subtree.

```text
taskcaged.service                  delegated root
├── manager                       taskcaged moves itself here
└── jobs                          controller-owning internal node
    ├── job-<id>                  leaf: launcher and target process tree
    └── job-<id>
```

Startup order is important.

1. systemd starts `taskcaged` with `Delegate=yes`.
2. The daemon discovers its service cgroup from `/proc/self/cgroup` unless an explicit test root is configured.
3. It creates `manager`, moves its own PID there and verifies the move.
4. It creates `jobs` and enables `+memory +cpu +pids` in the correct parent `cgroup.subtree_control`.
5. It verifies that it is the single writer for the delegated subtree.
6. It scans stale `job-*` leaves, kills any populated group and removes empty groups before accepting clients.

TaskCage never creates or modifies cgroups outside this resolved and canonicalized root.

## 4. Atomic process start

The Go daemon eliminates the post-start attach race with `clone3(CLONE_INTO_CGROUP)`.

1. Create `jobs/job-<id>` as an empty leaf cgroup.
2. Write and read back `memory.max`, `memory.swap.max`, `memory.oom.group`, `cpu.max` and `pids.max` as supported.
3. Capture baseline values from `memory.events.local`, `pids.events` and `cpu.stat`.
4. Open the job cgroup directory and retain its file descriptor.
5. Configure Go `exec.Cmd.SysProcAttr` with `UseCgroupFD`, `CgroupFD` and `PidFD`.
6. Start `taskcage-launcher -- <target> <args...>` directly inside the job cgroup.
7. The Rust launcher sets a parent-death signal and replaces itself with the target using a shell-free `exec`.
8. The target and every descendant inherit membership in the job cgroup.

There is no fallback that starts the target first and moves its PID afterward. If the kernel, filesystem or permissions cannot provide atomic entry, preflight returns `UNSUPPORTED` and the target is not executed.

The Rust launcher briefly contributes to job accounting before `exec`, but it has no runtime threads, network stack or long-lived heap and is replaced by the target in the same process. The Go runtime always stays outside the job cgroup.

## 5. Job lifecycle

```text
RECEIVED
   |
   v
QUEUED -- queue full/timeout --> REJECTED
   |
   v
PREPARING -- preflight/start error --> INTERNAL_ERROR or UNSUPPORTED
   |
   v
RUNNING -- limit/timeout/cancel --> KILLING
   |                                  |
   | normal exit                      v
   +----------------------------> COLLECTING
                                      |
                                      v
                                   CLEANUP
                                      |
                                      v
                                   RESULT
```

The execution permit is released only after cleanup reaches an empty cgroup or records an explicit cleanup failure. Permit ownership belongs to one lifecycle object and is released exactly once.

## 6. Limits and evidence

| User budget | Kernel or daemon mechanism | Evidence |
|---|---|---|
| memory | `memory.max` | `memory.events.local`, `memory.peak` |
| swap | `memory.swap.max` when supported | capability report and configured value |
| CPU rate | `cpu.max` | `cpu.stat` |
| process count | `pids.max` | `pids.events`, `pids.peak` when supported |
| wall time | Go monotonic timer | daemon watchdog state |
| output size | concurrent bounded stdout/stderr drain | collector state and byte counters |
| concurrency | daemon admission controller | active and queued counters |

Event files are read before execution and after termination. Classification uses counter deltas, not exit-code guesses.

Initial result-priority order:

1. explicit caller cancellation
2. queue capacity or queue timeout
3. wall timeout
4. output limit watchdog
5. memory OOM event delta
6. PID limit event delta
7. normal or non-zero target exit
8. target signal or unknown failure

## 7. Termination and cleanup

For timeout, cancellation or policy violation, the daemon performs the following sequence.

1. Record the watchdog or caller state that initiated termination.
2. Write `1` to the job's `cgroup.kill`.
3. Wait for `cgroup.events` to report `populated 0` within a bounded cleanup timeout.
4. Collect final kernel evidence and usage statistics.
5. Delete the empty job cgroup.
6. Return `ExecutionResult` and release the admission permit.

Killing only the launcher PID, target PID, process group or `ProcessHandle.descendants()` is never treated as successful cleanup.

## 8. Daemon crash recovery

- `taskcage-launcher` configures a parent-death signal so the target leader is killed if `taskcaged` dies.
- systemd supervises and restarts `taskcaged` on failure.
- startup scavenging treats every stale populated `job-*` group as unowned and invokes `cgroup.kill` before deletion.
- job metadata is written atomically outside the job leaf before target start and removed only after cleanup.
- the MVP may return an interrupted result to the Java caller after a daemon crash; cross-restart result replay is post-MVP.

The parent-death signal is defense in depth. Process-tree cleanup is proven by cgroup state, not by the signal alone.

## 9. Local protocol and trust model

- The daemon listens on `/run/taskcage/taskcaged.sock`.
- One connection carries one synchronous job request and final result.
- The protocol is versioned and length-prefixed to avoid newline and partial-read ambiguity.
- Socket file permissions and `SO_PEERCRED` restrict the MVP to the configured application account or group.
- Closing the connection before a result is interpreted as caller cancellation unless the job is already terminal.
- Commands are argv arrays. Shell command strings are not accepted.
- Working directories are canonicalized and validated against configured roots when that policy is enabled.

See [`PROTOCOL.md`](PROTOCOL.md) for the wire contract.

## 10. Security boundary

TaskCage limits resource consumption and cleans process trees. It does not isolate filesystem access, network access, system calls or kernel attack surface. The MVP does not use namespaces, seccomp, AppArmor or SELinux policy generation and must not be described as a security sandbox.

## 11. Dependency policy

### Go

- Prefer the standard library for UDS, JSON, timers, output collection and concurrency.
- Use `golang.org/x/sys/unix` only for Linux primitives not exposed by the standard library.
- Evaluate `github.com/containerd/cgroups/v3/cgroup2` for lifecycle helpers, while retaining direct TaskCage reads for kernel evidence and capability detection.
- Avoid gRPC, web frameworks and configuration frameworks in the MVP.

### Rust

- Use stable Rust edition 2024.
- Use `rustix` for Linux process controls and file-descriptor-safe syscall wrappers.
- Keep launcher dependencies minimal and produce a static musl release binary for the supported architecture.
- Do not move policy parsing, cgroup management or monitoring into the launcher.

### Java

- Use JDK 21 Unix Domain Socket APIs directly.
- Keep `taskcage-api` independent of Spring.
- Keep protocol transport in `taskcage-client`.
- Add Spring Boot auto-configuration only in `taskcage-spring-boot-starter`.

## 12. Compatibility target

The contest MVP pins one Ubuntu LTS x86-64 environment. Runtime preflight checks capabilities instead of trusting version strings:

- unified cgroup v2 mount
- delegated writable subtree
- `memory`, `cpu` and `pids` controllers
- `clone3(CLONE_INTO_CGROUP)` through Go `UseCgroupFD`
- `cgroup.kill`
- required statistic and event files

Broader distributions, ARM64 and a non-atomic shim fallback are post-MVP work.

## 13. Primary references

- [Linux cgroup v2](https://www.kernel.org/doc/html/latest/admin-guide/cgroup-v2.html)
- [systemd cgroup delegation](https://systemd.io/CGROUP_DELEGATION/)
- [Linux `clone3` and `CLONE_INTO_CGROUP`](https://www.man7.org/linux/man-pages/man2/clone3.2.html)
- [Go Linux process implementation and `UseCgroupFD`](https://go.dev/src/syscall/exec_linux.go)
- [Rust `rustix` process APIs](https://docs.rs/rustix/latest/rustix/process/)
- [Java 21 Unix Domain Socket API](https://docs.oracle.com/en/java/javase/21/docs/api/java.base/java/net/UnixDomainSocketAddress.html)
