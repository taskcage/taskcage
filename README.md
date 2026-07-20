# TaskCage

TaskCage safely runs resource-unpredictable external programs on Linux. A Go management daemon creates one cgroup v2 per job, a minimal Rust launcher starts the target inside that cgroup, and the Java SDK exposes the result to Spring applications.

> Project status: architecture and MVP scaffold. The executor is not ready for production use.

## MVP architecture

```text
Spring application
    |
    | Java 21 Unix Domain Socket client
    v
taskcaged (Go)
    |- admission control and bounded queue
    |- cgroup v2 lifecycle and resource limits
    |- output, timeout and kernel event monitoring
    `- termination classification and cleanup
             |
             | clone3(CLONE_INTO_CGROUP)
             v
      taskcage-launcher (Rust)
             `- exec target without a shell
```

The MVP supports one pinned Ubuntu LTS/x86-64 environment with cgroup v2. It is a resource isolation tool, not a security sandbox.

## Repository layout

| Path | Responsibility |
|---|---|
| `cmd/taskcaged` | Go daemon entry point |
| `internal` | Go scheduler, cgroup manager, executor, monitor and protocol |
| `crates/taskcage-launcher` | Minimal Rust exec boundary |
| `crates/taskcage-fixtures` | Safe native integration-test fixtures |
| `sdk/java` | Java API, UDS client and Spring Boot Starter |
| `deploy/systemd` | Delegated cgroup service configuration |
| `tests/integration` | Real Linux cgroup v2 tests |
| `docs` | PRD, architecture, protocol, ADRs and MVP roadmap |

## Documents

- [Product requirements](docs/PRD.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Local protocol](docs/PROTOCOL.md)
- [Two-person MVP roadmap](docs/MVP-ROADMAP.md)

## Toolchains

- Go 1.25+; CI should use the latest supported patch release
- Rust stable, edition 2024
- Java 21+
- Gradle 9+
- Linux kernel with cgroup v2, `clone3(CLONE_INTO_CGROUP)` and `cgroup.kill`

Build and integration commands will be added as each component becomes executable. Linux integration tests must run on a real delegated cgroup v2 environment and fail closed elsewhere.
