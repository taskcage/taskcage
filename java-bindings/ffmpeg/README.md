# TaskCage FFmpeg Binding

This Java 17+ library maps one typed audio-to-WAV operation to the installed
`ffmpeg-audio-to-wav@1.0.0` Execution Profile. It is a separate convenience layer over the
TaskCage Java Core SDK and does not expose an FFmpeg executable path or caller-provided argv.

The next release coordinates are:

```kotlin
dependencies {
    implementation("org.taskcage:taskcage-ffmpeg-binding:0.1.0")
}
```

The Binding depends transitively on `org.taskcage:taskcage-java-sdk:0.3.0` and requires taskcaged
`0.4.0` with the matching signed Bundle and verified FFmpeg Runtime Package imported. These
coordinates are available from Maven Central and GitHub Release artifacts.

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

The Binding requires a daemon with the matching signed Bundle and verified FFmpeg Runtime Package.
The repository's development container imports both into a local catalog and runs the Java-to-daemon
E2E. A production deployment imports its approved Bundle and Package artifacts using the daemon's
Bundle and Package commands.

Run the local unit tests from the repository root:

```bash
./java-sdk/gradlew -p java-bindings/ffmpeg test
```

Run the real Binding workflow on an x86-64 Docker host with cgroup v2:

```bash
bash dev/container/run-e2e.sh
```

A copyable application is available in [`examples/ffmpeg-java`](../../examples/ffmpeg-java/README.md).
