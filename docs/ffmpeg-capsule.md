# FFmpeg Audio-to-WAV Capsule

`ffmpeg-audio-to-wav@1.0.0`은 TaskCage의 첫 reference Capsule이다. FFmpeg 전체 option을 Java API로
복제하지 않고, 하나의 작은 변환 capability를 Capsule schema로 제공한다.

Java 호출자는 executable path, argv, working directory, environment, output file name을 전달하지 않는다.
Core SDK의 generic `CapsuleRequest`로 Capsule 이름과 선언된 input만 제공한다.

```java
CapsuleRequest request = CapsuleRequest.builder("ffmpeg-audio-to-wav", "1.0.0")
    .artifact("source", source)
    .int64("sample_rate_hz", 16_000)
    .int64("channels", 1)
    .build();
```

daemon은 Capsule signature, Runtime Package digest, 플랫폼, input schema와 resource override를 검증한 뒤
선언된 argv를 materialize한다. Core SDK가 daemon의 trust boundary를 대체하지 않는다.

## 실행 계약

첫 작업은 입력 media의 첫 audio stream을 PCM WAVE로 변환한다.

| 항목 | 값 |
| --- | --- |
| Capsule identity | `ffmpeg-audio-to-wav@1.0.0` |
| input Artifact | `source` (required) |
| sample rate | `sample_rate_hz`: `8000`, `16000`, `22050`, `44100`, `48000` |
| channels | `channels`: `1` 또는 `2` |
| output Artifact | `audio` |
| output media type | `audio/wav` |
| codec | `pcm_s16le` |

개념적인 argv는 다음 의미를 가진다. 실제 executable과 staging path는 daemon이 검증된 Runtime Package와
Artifact staging에서 결정한다.

```text
ffmpeg
  -hide_banner -loglevel error -nostdin
  -i <staged-source>
  -map 0:a:0 -vn
  -c:a pcm_s16le
  -ar <sample-rate-hz>
  -ac <channels>
  <staged-result.wav>
```

shell, PATH lookup, caller-provided option, caller-provided output path는 사용하지 않는다. 입력에 audio
stream이 없거나 FFmpeg가 non-zero로 종료하면 output Artifact를 publish하지 않는다.

## 검증

[`examples/ffmpeg-java`](../examples/ffmpeg-java/README.md)는 generic Capsule API로 변환과 결과 Artifact
확인을 보인다. Compose E2E는 정상 실행, timeout, cancel, memory/PID limit, output validation과 whole-task
cleanup을 검증한다.
