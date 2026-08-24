# TaskCage daemon container

This directory contains the production container definition for `taskcaged`. It is deliberately
separate from [`dev/container`](../../../dev/container), which builds FFmpeg, test fixtures, and
development-only signing material for E2E testing.

The published image contains only the daemon and its minimum OS dependency. It does **not** include
a Runtime Package, Capsule, development credentials, or test fixtures.

## Quick start without bind mounts

Like MySQL's automatic SSL setup, the image creates a self-signed certificate and Remote configuration when no
explicit Remote configuration is supplied. Only the service-account secret is supplied; no host directory is
mounted.

```bash
docker run --detach \
  --name taskcaged-local \
  --privileged \
  --cgroupns private \
  -p 7443:7443 \
  -e TASKCAGE_CLIENT_SECRET='replace-with-a-long-random-secret' \
  ghcr.io/taskcage/taskcaged:<release-version>
```

The generated configuration remains in the container's writable layer across `docker restart`, but is removed by
`docker rm`. Add `--mount type=volume,src=taskcage-data,dst=/var/lib/taskcage` when local Capsules and Runtime
Packages should survive container replacement. This is a Docker-managed named volume, not a caller file mount.

This default always uses TLS but does not establish a CA trust relationship. It is appropriate only when the
network path is already trusted. Use a long secret and use an explicit CA configuration for an untrusted or
production network. Connect with client ID `taskcage` and the same `TASKCAGE_CLIENT_SECRET` value.

```java
try (RemoteTaskCageClient client = RemoteTaskCageClient.localDefault(
        ServiceCredentials.of("taskcage", Secret.fromEnvironment("TASKCAGE_CLIENT_SECRET")))) {
    // RemoteCapsuleRunner.external(client)
}
```

## Run an image

The production image is a TLS service. It does not trust arbitrary certificates, generate reusable server
credentials, or allow unauthenticated clients. Provide a Remote configuration and server certificate, then
publish the default container port with normal Docker syntax. The Java client can use `PREFERRED`, CA, or hostname
verification according to the network trust boundary.

```bash
export TASKCAGE_IMAGE=ghcr.io/taskcage/taskcaged
export TASKCAGE_VERSION=<release-containing-tls-container-support>

docker run --detach \
  --name taskcaged \
  --privileged \
  --cgroupns private \
  -p 7443:7443 \
  --mount type=volume,src=taskcage-data,dst=/var/lib/taskcage \
  --mount type=bind,src="$PWD/taskcage-remote.json",dst=/bootstrap/remote.json,readonly \
  --mount type=bind,src="$PWD/tls",dst=/bootstrap/tls,readonly \
  "${TASKCAGE_IMAGE}:${TASKCAGE_VERSION}"
```

`-p 9443:7443` changes only the host port. Clients then connect to
`taskcage+tls://host:9443`. The container's TLS listener remains on port `7443`.

The `taskcage-remote.json` file uses the standard [Remote daemon configuration](../../REMOTE.md). Its
certificate and private-key paths must refer to `/var/lib/taskcage/config/tls/chain.pem` and
`/var/lib/taskcage/config/tls/private-key.pem`. On first startup, the entrypoint copies the bootstrap files into
the named `taskcage-data` volume with the restrictive ownership and mode required by the daemon. Later restarts
need only the named volume. In production, supply bootstrap files through the platform's secret mechanism rather
than embedding them in the image or passing secrets in command arguments.

The named volume persists daemon configuration, imported Capsules and Runtime Packages. It is not used to exchange
application input or output files. The daemon keeps the legacy UDS listener internally for existing host
installations. Public container clients should use the authenticated TLS listener; they do not mount a socket or
share caller-owned Artifact paths.

`--privileged` and `--cgroupns private` are required for TaskCage to create cgroup v2 boundaries.
Run this only on a trusted Linux Docker host. This image is not a security sandbox for untrusted code.

At a minimum, set `listenAddress` to `0.0.0.0:7443`, point `artifactRoot` under
`/var/lib/taskcage`, and use the copied certificate paths above. The daemon rejects unsafe key ownership and
permissions instead of starting with weaker TLS settings.

The certificate's Subject Alternative Name must include the hostname used by a `VERIFY_IDENTITY` client (for
example, `taskcage.internal` in production). `VERIFY_CA` and `VERIFY_IDENTITY` trust the issuing CA through the
JVM trust store or an explicitly configured trust store. `PREFERRED` keeps TLS encryption but does not require a
CA; its service-account secret still authenticates the caller.

## Java client files

`RemoteCapsuleFileRequest` uploads a caller `Path` over the authenticated TLS connection and downloads the
verified output Artifact to a caller `Path`. The input and output files therefore do not require Docker bind
mounts or container-internal paths.

```java
try (RemoteTaskCageClient client = RemoteTaskCageClient.localDefault(credentials)) {
    RemoteCapsuleExecutionResult result = RemoteCapsuleRunner.external(client).execute(request, timeout);
}
```

## Prepare a Capsule

Before starting the daemon, install a verified Capsule Pack into the same data volume. The Pack and
public key below are supplied by your organization or a future Capsule registry; they are intentionally
not baked into the daemon image.

```bash
docker run --rm \
  -v taskcage-data:/var/lib/taskcage \
  -v "$PWD/import:/import:ro" \
  "${TASKCAGE_IMAGE}:${TASKCAGE_VERSION}" \
  taskcaged capsule install \
    /import/ffmpeg-audio-to-wav-1.0.0.tccapsule.tar.gz
```

The Pack's Runtime Package must be compatible with the container's Linux CPU architecture. The image looks up the
Pack signing key from `/etc/taskcage/trusted-capsules.d`; an official image Release will place its official key there.
For an organization key, mount that directory read-only or pass an explicit `--trust-store` during install.

## Publishing

The repository workflow `Publish taskcaged container` builds native Linux AMD64 and ARM64 images for
a signed `taskcaged-vX.Y.Z` tag, then publishes one multi-platform manifest at
`ghcr.io/taskcage/taskcaged:X.Y.Z`. Before its first run, allow GitHub Actions to write packages for
this repository. Make the resulting GitHub Container Registry package public if users should pull it
without credentials.

## Local image check

Build the production image from this repository:

```bash
docker build \
  --file daemon/deploy/container/Dockerfile \
  --tag taskcage-local:dev \
  .
```

Run the local image with the same TLS configuration:

```bash
docker run --detach \
  --name taskcaged-local \
  --privileged \
  --cgroupns private \
  -p 7443:7443 \
  --mount type=volume,src=taskcage-data,dst=/var/lib/taskcage \
  --mount type=bind,src="$PWD/taskcage-remote.json",dst=/bootstrap/remote.json,readonly \
  --mount type=bind,src="$PWD/tls",dst=/bootstrap/tls,readonly \
  taskcage-local:dev
```
