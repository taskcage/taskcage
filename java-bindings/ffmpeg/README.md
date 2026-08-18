# TaskCage FFmpeg Binding

This Java 17+ library maps one typed audio-to-WAV operation to the installed
`ffmpeg-audio-to-wav@1.0.0` Execution Profile. It is a separate convenience layer over the
TaskCage Java Core SDK and does not expose an FFmpeg executable path or caller-provided argv.

Version `0.1.0` is published at these coordinates:

```kotlin
dependencies {
    implementation("org.taskcage:taskcage-ffmpeg-binding:0.1.0")
}
```

The published Binding depends transitively on `org.taskcage:taskcage-java-sdk:0.2.0`. It works with
the released taskcaged `0.4.0` when the matching static FFmpeg Profile and verified Runtime Package
are configured; daemon `0.4.0` does not require a signed Bundle and does not provide the `bundle`
command or `--bundle-cache-root`.

```java
FfmpegAudioToWavRequest request = new FfmpegAudioToWavRequest(
    source,
    AudioSampleRate.HZ_16000,
    AudioChannels.MONO);

FfmpegAudioToWavResult result = FfmpegAudioToWavBinding.using(taskCage)
    .run(request, Duration.ofMinutes(2));

if (result instanceof FfmpegAudioToWavSuccess success) {
    PublishedArtifact audio = success.audio();
} else if (result instanceof FfmpegAudioToWavFailure failure) {
    ProfileFailure cause = failure.failure();
}
```

`TaskCageClient` implements the smaller `ProfileRuntime` contract used internally by the Binding.
Existing `using(TaskCageClient)` calls remain supported, while adapters and tests can supply only a
`ProfileRuntime`. The Binding never closes either form; connection and runtime lifecycle remain
caller-owned. Both forms retain the overload that accepts a caller-owned `clientRequestId`.

## Released daemon 0.4.0 setup

Import the FFmpeg Runtime Package as the daemon service UID, then register its digest with the two
static Profile options alongside the required Artifact options:

```bash
sudo -u taskcage taskcaged import-package \
  --source /srv/taskcage-import/ffmpeg-7.1.1 \
  --cache-root /var/lib/taskcage

taskcaged serve \
  <required serve and Artifact options> \
  --runtime-package-cache-root /var/lib/taskcage \
  --ffmpeg-audio-to-wav-package-digest sha256:<64-lowercase-hex>
```

See the [Runtime Package cache contract](../../docs/runtime-package-cache.md) for the package and
static registration requirements.

## Bundle catalog on `main` (not yet released)

After the `taskcaged-v0.4.0` tag, `main` added signed Bundle import, the local catalog and
`--bundle-cache-root`. No public daemon release contains that path yet, and no minimum release
version has been assigned. The repository's current development container imports the Package and
Bundle into the local catalog for its Java-to-daemon E2E; released daemon `0.4.0` deployments must
use the static Profile setup above. The `main`-only contract is documented in
[TaskCage Bundle format v0alpha1](../../docs/bundle-format.md).

Run the local unit tests from the repository root:

```bash
./java-sdk/gradlew -p java-bindings/ffmpeg test
```

Run the real Binding workflow on an x86-64 Docker host with cgroup v2:

```bash
bash dev/container/run-e2e.sh
```

A copyable application is available in [`examples/ffmpeg-java`](../../examples/ffmpeg-java/README.md).
