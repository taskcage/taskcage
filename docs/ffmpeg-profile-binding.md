# FFmpeg Audio-to-WAV Capsule convenience API

> 상태: 이 문서는 기존 `taskcage-ffmpeg-binding` artifact의 호환 경로를 설명한다. 다음 Capsule-first
> 공개 계약에서는 프로세스별 Binding을 필수 개념으로 만들지 않고, 선언된 Profile schema를 Java SDK의
> typed input/output API로 노출한다. 실제 사용자 경로는 container Java-to-daemon E2E와
> [`examples/ffmpeg-java`](../examples/ffmpeg-java/)로 검증한다.

## 목적

첫 FFmpeg convenience API는 Java 사용자가 실행 파일 경로와 argv를 만들지 않고, 하나의 타입 안전한 작업으로
FFmpeg를 실행하는 경험을 검증한다.

```java
FfmpegAudioToWavRequest request = new FfmpegAudioToWavRequest(
    source,
    AudioSampleRate.HZ_16000,
    AudioChannels.MONO);

try (TaskCageClient taskCage = TaskCageClient.connect(config)) {
    FfmpegAudioToWavResult result = FfmpegAudioToWavBinding.using(taskCage)
        .run(request, Duration.ofMinutes(2));
}
```

이 API는 별도 Java 편의 계층이다. Task 제출·조회·취소, transport, Profile wire model과 input/output data
type은 기존 TaskCage Core SDK가 계속 소유한다.

이 API는 `ffmpeg-audio-to-wav@1.0.0` Capsule archive를 설치한 daemon에 generic `ProfileRequest`를 보낸다. Capsule은
FFmpeg Runtime Package digest와 고정 argv·input/output·정책 계약을 제공한다. 현재 convenience artifact는
기존 사용자를 위해 독립 Java package로 유지한다.

## 확정된 경계

- 첫 convenience API는 FFmpeg CLI 전체가 아니라 하나의 고정된 작업만 제공한다.
- caller는 executable path, argv, working directory, environment 또는 output file name을 지정하지 않는다.
- Binding은 하나의 고정된 Profile name/version과 input slot으로 `ProfileRequest`를 결정적으로 만든다.
- 입력과 출력은 승인된 Local Artifact 계약을 사용한다.
- 자원 override는 Core SDK의 `ProfileResourceOverrides`를 그대로 사용하며 Binding이 daemon 정책을
  추측하지 않는다.
- Binding은 Java Core SDK artifact에 포함하지 않고 별도 artifact와 package로 제공한다.
- Runtime Package와 signed Bundle import/cache는 daemon이 소유한다.
- Hub, Remote transport, URL input, 여러 input/output, 전체 FFmpeg option 노출은 포함하지 않는다.

Binding은 daemon의 trust boundary가 아니다. Binding이 만든 `ProfileRequest`도 daemon이 설치된 Profile,
허용된 Package digest, Artifact와 resource override 정책을 다시 검증한다.

## 실행 계약

첫 작업은 입력 media의 첫 audio stream을 고정된 PCM WAVE 결과로 변환한다. codec 선택과 container
format을 사용자가 자유 문자열로 전달하지 않으므로 Profile이 작은 허용 목록과 안전한 argv를 유지할 수
있다.

| 항목 | 확정 값 |
|---|---|
| Profile identity | `ffmpeg-audio-to-wav@1.0.0` |
| input Artifact slot | `source` (`LOCAL_INPUT`, required) |
| sample rate slot | `sample_rate_hz` (`INT64`, required) |
| channel slot | `channels` (`INT64`, required) |
| output slot | `audio` |
| fixed output file | `result.wav` |
| media type | `audio/wav` |
| codec | signed 16-bit little-endian PCM (`pcm_s16le`) |

허용 sample rate는 `8000`, `16000`, `22050`, `44100`, `48000` Hz로 제한하고 channel 수는 `1` 또는
`2`만 허용한다. Profile은 input의 첫 audio stream 하나를 선택하고 video와 그 밖의 stream은 output에
포함하지 않는다.

정적 daemon 등록 경로와 Bundle catalog 경로는 같은 immutable `ffmpeg-audio-to-wav@1.0.0` 입력 계약을
사용한다. Bundle `profile.json`은 `sample_rate_hz`에
`allowedValues: [8000, 16000, 22050, 44100, 48000]`, `channels`에 `allowedValues: [1, 2]`를 선언한다.
따라서 범위 안이더라도 목록에 없는 값(예: `12345`)이나 channel `3`은 두 경로 모두
`INVALID_PROFILE_INPUT`으로 거절한다.

개념적인 argv는 다음 의미를 가져야 한다. 실제 executable과 staging path는 daemon이 검증된 Runtime
Package와 Artifact staging에서 resolve한다.

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

shell, PATH lookup, caller-provided option, caller-provided output path는 사용하지 않는다. 입력에 audio stream이
없거나 FFmpeg가 non-zero로 끝나면 Profile result는 기존 Core 계약의 `FAILED`이며 output Artifact를
publish하지 않는다.

## Java Binding 경계

별도 배포 단위는 다음과 같다.

| 항목 | 확정 값 |
|---|---|
| Maven artifact | `org.taskcage:taskcage-ffmpeg-binding` |
| Java package | `org.taskcage.binding.ffmpeg` |
| Core dependency | `org.taskcage:taskcage-java-sdk` |
| 공개 진입점 | `FfmpegAudioToWavBinding` |
| request | `FfmpegAudioToWavRequest` |
| result | `FfmpegAudioToWavResult` |

Binding release는 지원하는 `ffmpeg-audio-to-wav` Profile version 범위와 필요한 Core SDK·Protocol version을
명시해야 한다. Bundle과 Binding은 독립 버전으로 배포되며, Binding이 없더라도 같은 Profile은 Core SDK의
generic `ProfileRequest`로 실행할 수 있어야 한다.

Binding request는 `LocalInputArtifact`, `AudioSampleRate`, `AudioChannels`와 선택적인
`ProfileResourceOverrides`만 받는다. Binding은 아래 내용을 고정한다.

- `ProfileIdentity("ffmpeg-audio-to-wav", "1.0.0")`
- `source`, `sample_rate_hz`, `channels` slot 이름과 Core input value 변환
- 성공 result의 `audio` output slot 존재와 `audio/wav` media type 검증

result는 `FfmpegAudioToWavSuccess`와 `FfmpegAudioToWavFailure`의 sealed union이다. 성공 result만
`PublishedArtifact audio()`를 제공하고, 두 result 모두 `FinishedProfileTaskSnapshot task()`를 제공한다.
따라서 외부 프로세스 실패를 SDK 통신 예외로 바꾸지 않으면서 일반 사용자는 타입 안전한 output을 받고,
고급 사용자는 종료 원인·사용량·출력 tail 같은 Core 결과도 잃지 않는다.

Binding은 Core client를 소유하지 않는다. caller가 전달한 `TaskCageClient`의 lifecycle은 caller 책임이며,
Binding `close()`나 별도 connection pool을 추가하지 않는다.

## 구현과 협업 순서

1. Java Binding module이 typed request mapping과 result validation을 제공한다.
2. daemon이 Runtime Package import/cache, verified executable resolution과 고정 Profile 계약을 소유한다.
3. container 개발 환경이 FFmpeg Package를 import하고 Java-to-daemon 실제 FFmpeg E2E와 복사 가능한
   예제를 실행한다.

## 완료 기준

- Java 호출자가 executable path와 argv 없이 audio-to-WAV 작업을 실행한다.
- 같은 typed request는 byte-equivalent Core `ProfileRequest`로 변환된다.
- 알 수 없는 output slot, 누락된 `audio` Artifact 또는 잘못된 media type을 Binding이 protocol/result
  오류로 거절한다.
- 실제 FFmpeg Runtime Package가 digest로 검증된 뒤에만 Task가 시작된다.
- Java-to-daemon E2E가 정상 WAVE header, Profile identity, published Artifact digest와 cleanup을 확인한다.
- 문서 예제가 container 개발 환경에서 재현된다.

## 승인된 결정

2026-08-12에 다음 세 항목을 구현 기준으로 승인했다.

1. 첫 작업을 범용 video transcode가 아니라 `audio-to-WAV`로 제한한다.
2. 출력은 `pcm_s16le`, `result.wav`, `audio/wav`로 고정한다.
3. 사용자가 고르는 값은 sample rate와 mono/stereo만 제공한다.
