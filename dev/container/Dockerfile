# syntax=docker/dockerfile:1

FROM rust:1.88-bookworm AS rust-builder

WORKDIR /workspace
COPY Cargo.toml Cargo.lock ./
COPY daemon ./daemon
COPY test-fixtures ./test-fixtures
RUN cargo build --workspace --release --locked

FROM ubuntu:24.04 AS daemon

COPY --from=rust-builder /workspace/target/release/taskcaged /usr/local/bin/taskcaged
COPY --from=rust-builder /workspace/target/release/ghost-tree /usr/local/libexec/taskcage/ghost-tree
COPY --from=rust-builder /workspace/target/release/output-flood /usr/local/libexec/taskcage/output-flood
COPY --chmod=0755 dev/container/entrypoint.sh /usr/local/bin/taskcage-container-entrypoint
COPY --chmod=0755 dev/container/verify-cleanup.sh /usr/local/bin/taskcage-container-verify-cleanup

ENV RUST_LOG=taskcaged=info \
    TASKCAGE_LOG_FORMAT=json

ENTRYPOINT ["/usr/local/bin/taskcage-container-entrypoint"]
CMD ["serve", \
    "--socket", "/run/taskcage/taskcaged.sock", \
    "--max-concurrent-tasks", "4", \
    "--max-registry-tasks", "1000", \
    "--max-concurrent-connections", "32", \
    "--cleanup-timeout-ms", "5000", \
    "--fail-stop-timeout-ms", "10000", \
    "--max-task-cpu-quota-us", "200000", \
    "--max-task-cpu-period-us", "100000", \
    "--max-task-memory-bytes", "2147483648", \
    "--max-task-pids", "128", \
    "--max-task-timeout-ms", "900000", \
    "--max-task-stdout-tail-bytes", "65536", \
    "--max-task-stderr-tail-bytes", "65536"]

FROM eclipse-temurin:17-jdk-jammy AS java-e2e

WORKDIR /workspace
COPY java-sdk ./java-sdk
COPY protocol-fixtures ./protocol-fixtures
RUN ./java-sdk/gradlew -p ./java-sdk test e2eTestClasses --no-daemon

WORKDIR /workspace/java-sdk
CMD ["./gradlew", "test", "e2eTest", "--rerun-tasks", "--no-daemon", "--offline"]
