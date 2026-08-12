# TaskCage FFmpeg Binding

This Java 17+ library maps one typed audio-to-WAV operation to the installed
`ffmpeg-audio-to-wav@1.0.0` Execution Profile. It is a separate convenience layer over the
TaskCage Java Core SDK and does not expose an FFmpeg executable path or caller-provided argv.

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

The Binding request mapping and result validation are available independently of the daemon
Runtime Package work. A real FFmpeg execution requires a daemon with the matching Profile and its
verified Runtime Package installed; that integration remains tracked by issues #151 and #154.

Run the local unit tests from the repository root:

```bash
./java-sdk/gradlew -p java-bindings/ffmpeg test
```
