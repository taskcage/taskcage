# syntax=docker/dockerfile:1

FROM rust:1.88-bookworm AS rust-builder

WORKDIR /workspace
COPY Cargo.toml Cargo.lock ./
COPY daemon ./daemon
COPY test-fixtures ./test-fixtures
RUN cargo build --workspace --release --locked

FROM ubuntu:24.04 AS daemon

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates ffmpeg \
    && rm -rf /var/lib/apt/lists/*

COPY --from=rust-builder /workspace/target/release/taskcaged /usr/local/bin/taskcaged
COPY --from=rust-builder /workspace/target/release/ghost-tree /usr/local/libexec/taskcage/ghost-tree
COPY --from=rust-builder /workspace/target/release/output-flood /usr/local/libexec/taskcage/output-flood
COPY --from=rust-builder /workspace/target/release/memory-hog /usr/local/libexec/taskcage/memory-hog
COPY daemon/tests/data/remote-chain.pem daemon/tests/data/remote-end.key daemon/tests/data/remote-root.pem /usr/local/share/taskcage/
COPY --chmod=0755 dev/container/entrypoint.sh /usr/local/bin/taskcage-container-entrypoint
COPY --chmod=0755 dev/container/remote-entrypoint.sh /usr/local/bin/taskcage-container-remote-entrypoint
COPY --chmod=0755 dev/container/prepare-ffmpeg-package.sh /usr/local/bin/taskcage-container-prepare-ffmpeg-package
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
    "--max-task-stderr-tail-bytes", "65536", \
    "--profile-artifact-root", "/taskcage-work/artifacts", \
    "--profile-artifact-max-bytes", "104857600"]

FROM eclipse-temurin:17-jdk-jammy AS java-e2e

WORKDIR /workspace
COPY java-sdk ./java-sdk
COPY java-bindings ./java-bindings
COPY protocol-fixtures ./protocol-fixtures
COPY daemon/tests/data/remote-root.pem ./daemon/tests/data/remote-root.pem
RUN ./java-sdk/gradlew -p ./java-sdk test e2eTestClasses remoteE2eTestClasses --no-daemon \
    && ./java-sdk/gradlew -p ./java-bindings/ffmpeg test bindingE2eTestClasses --no-daemon

WORKDIR /workspace
CMD ["/bin/bash", "-c", "./java-sdk/gradlew -p ./java-sdk test e2eTest --rerun-tasks --no-daemon --offline && ./java-sdk/gradlew -p ./java-bindings/ffmpeg test bindingE2eTest --rerun-tasks --no-daemon --offline"]

FROM java-e2e AS java-remote-e2e

CMD ["/bin/bash", "-c", "./java-sdk/gradlew -p ./java-sdk remoteE2eTest --rerun-tasks --no-daemon --offline"]

FROM eclipse-temurin:17-jdk-jammy AS ffmpeg-example

WORKDIR /workspace
COPY java-sdk ./java-sdk
COPY java-bindings ./java-bindings
COPY examples/ffmpeg-java ./examples/ffmpeg-java
RUN ./java-sdk/gradlew -p ./examples/ffmpeg-java installDist --no-daemon

CMD ["/bin/bash", "-c", "java -cp '/workspace/examples/ffmpeg-java/build/install/taskcage-ffmpeg-java-example/lib/*' org.taskcage.example.ffmpeg.GenerateInput /taskcage-work/artifacts/example/source.wav && exec /workspace/examples/ffmpeg-java/build/install/taskcage-ffmpeg-java-example/bin/taskcage-ffmpeg-java-example /run/taskcage/taskcaged.sock /taskcage-work/artifacts example/source.wav"]

FROM eclipse-temurin:17-jdk-jammy AS benchmark-builder

WORKDIR /workspace
COPY java-sdk ./java-sdk
COPY dev/benchmark/runner ./dev/benchmark/runner
RUN ./java-sdk/gradlew -p ./dev/benchmark/runner installDist --no-daemon

FROM daemon AS benchmark-worker

RUN apt-get update \
    && apt-get install -y --no-install-recommends openjdk-17-jre-headless \
    && rm -rf /var/lib/apt/lists/*

COPY --from=benchmark-builder /workspace/dev/benchmark/runner/build/install/taskcage-execution-worker-benchmark /opt/taskcage-benchmark

ENTRYPOINT ["/opt/taskcage-benchmark/bin/taskcage-execution-worker-benchmark"]
