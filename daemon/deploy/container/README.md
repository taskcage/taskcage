# TaskCage daemon container

This directory contains the production container definition for `taskcaged`. It is deliberately
separate from [`dev/container`](../../../dev/container), which builds FFmpeg, test fixtures, and
development-only signing material for E2E testing.

The published image contains only the daemon and its minimum OS dependency. It does **not** include
a Runtime Package, Capsule, development credentials, or test fixtures.

## Run an image

After the matching daemon release is published to GHCR, pin the image version and start it with:

```bash
cd daemon/deploy/container
export TASKCAGE_VERSION=0.6.1 # Replace with a published daemon version.
docker compose up --detach --wait
```

The daemon uses a Unix domain socket at `/run/taskcage/taskcaged.sock`. A Java Worker container that
uses the Local SDK must mount both named volumes: `taskcage-runtime` for the socket and
`taskcage-data` for caller-owned Artifacts.

`privileged: true` and `cgroup: private` are required for TaskCage to create cgroup v2 boundaries.
Run this only on a trusted Linux Docker host. This image is not a security sandbox for untrusted code.

## Prepare a Capsule

Before starting the daemon, import a verified Runtime Package and Capsule into the same data volume.
The archive and public key below are supplied by your organization or a future Capsule registry;
they are intentionally not baked into the daemon image.

```bash
export TASKCAGE_IMAGE=ghcr.io/taskcage/taskcaged
export TASKCAGE_VERSION=0.6.1 # Replace with a published daemon version.

docker run --rm \
  -v taskcage_taskcage-data:/var/lib/taskcage \
  -v "$PWD/import:/import:ro" \
  "${TASKCAGE_IMAGE}:${TASKCAGE_VERSION}" \
  taskcaged import-package \
    --source /import/ffmpeg-runtime \
    --cache-root /var/lib/taskcage/runtime-package-cache

docker run --rm \
  -v taskcage_taskcage-data:/var/lib/taskcage \
  -v "$PWD/import:/import:ro" \
  "${TASKCAGE_IMAGE}:${TASKCAGE_VERSION}" \
  taskcaged bundle import \
    --source /import/ffmpeg-audio-to-wav-1.0.0.tcbundle.tar.gz \
    --cache-root /var/lib/taskcage/runtime-package-cache \
    --trusted-key taskcage-release=/import/taskcage-release.pub
```

The Runtime Package and Capsule must be compatible with the container's Linux CPU architecture.

## Local image check

Build the production image from this repository:

```bash
docker build \
  --file daemon/deploy/container/Dockerfile \
  --tag taskcage-local:0.6.0 \
  .
```

Use the local image without editing the Compose file:

```bash
TASKCAGE_IMAGE=taskcage-local TASKCAGE_VERSION=0.6.0 docker compose up --detach --wait
```
