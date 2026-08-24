# syntax=docker/dockerfile:1

FROM rust:1.88-bookworm AS builder

WORKDIR /workspace
COPY Cargo.toml Cargo.lock ./
COPY core ./core
COPY linux-runtime ./linux-runtime
COPY daemon ./daemon
COPY test-fixtures ./test-fixtures
RUN cargo build --package taskcaged --release --locked

FROM ubuntu:24.04

ARG VERSION=dev
ARG REVISION=unknown

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates openssl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /workspace/target/release/taskcaged /usr/local/bin/taskcaged
COPY --chmod=0755 daemon/deploy/container/entrypoint.sh /usr/local/bin/taskcaged-container-entrypoint

LABEL org.opencontainers.image.title="TaskCage daemon" \
    org.opencontainers.image.description="Linux cgroup v2 runtime for trusted external CLI tasks" \
    org.opencontainers.image.source="https://github.com/taskcage/taskcage" \
    org.opencontainers.image.version="${VERSION}" \
    org.opencontainers.image.revision="${REVISION}"

ENV RUST_LOG=taskcaged=info \
    TASKCAGE_LOG_FORMAT=json \
    TASKCAGE_REMOTE_BOOTSTRAP_DIR=/bootstrap

# The public container is a TLS service. Docker does not publish this port unless the
# operator explicitly supplies `-p <host-port>:7443`.
EXPOSE 7443

ENTRYPOINT ["/usr/local/bin/taskcaged-container-entrypoint"]
CMD ["taskcaged", "serve", \
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
    "--max-task-stderr-tail-bytes", "65536", \
    "--profile-artifact-root", "/var/lib/taskcage/artifacts", \
    "--profile-artifact-max-bytes", "104857600", \
    "--bundle-cache-root", "/var/lib/taskcage/runtime-package-cache"]

HEALTHCHECK --interval=5s --timeout=2s --retries=12 \
    CMD ["taskcaged", "status", "--socket", "/run/taskcage/taskcaged.sock", "--timeout-ms", "1000"]
