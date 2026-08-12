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

The Binding depends transitively on `org.taskcage:taskcage-java-sdk:0.2.0` and requires taskcaged
`0.2.0` with the matching Profile configured. These coordinates become installable after their
Maven Central releases complete.

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

The Binding requires a daemon with the matching Profile and a verified FFmpeg Runtime Package.
The repository's development container prepares that package and runs the Java-to-daemon E2E.

Run the local unit tests from the repository root:

```bash
./java-sdk/gradlew -p java-bindings/ffmpeg test
```

Run the real Binding workflow on an x86-64 Docker host with cgroup v2:

```bash
bash dev/container/run-e2e.sh
```

A copyable application is available in [`examples/ffmpeg-java`](../../examples/ffmpeg-java/README.md).
