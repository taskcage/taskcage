# Java FFmpeg Capsule example

This example converts one caller-owned audio file to mono 16 kHz PCM WAVE through the installed
`ffmpeg-audio-to-wav@1.0.0` Capsule. The application does not supply an FFmpeg executable path,
argv, working directory, environment, or output filename.

From the repository root, run the complete development environment and example with:

```bash
bash dev/container/run-ffmpeg-example.sh
```

The command builds the daemon and Java SDK, imports the container's FFmpeg binary as a verified
Runtime Package, starts the matching Capsule, generates an input WAVE file, and prints the
published result path. It then removes all containers and volumes.

The development container requires an x86-64 Docker host with cgroup v2. It uses
`privileged: true` and host PID sharing solely to exercise TaskCage's cgroup lifecycle; do not use
this Compose setup in production or on an untrusted machine.

The application entry point is
[`FfmpegExample.java`](src/main/java/org/taskcage/example/ffmpeg/FfmpegExample.java).

The repository build uses a Gradle composite build so it can validate unreleased source. An external
application needs only the Core SDK dependency.

```kotlin
dependencies {
    implementation("org.taskcage:taskcage-java-sdk:0.4.0")
}
```
