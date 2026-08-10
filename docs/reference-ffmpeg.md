# FFmpeg Local Raw Command reference workflow

이 문서는 Local Public Alpha의 첫 실제 도구 경로를 FFmpeg 하나로 검증한다. Ubuntu package의 FFmpeg를
절대 경로와 argv 배열로 제출하고, 설치된 `taskcaged` systemd service와 Java Core SDK가 정상 결과와
timeout whole-task cleanup을 같은 Task 계약으로 반환하는지 확인한다.

이 workflow는 correctness evidence다. Docker와의 시작 시간, memory, disk 또는 처리량을 측정하지 않으며
TaskCage가 더 빠르거나 가볍다는 성능 주장의 근거로 사용하지 않는다.

## 전용 Ubuntu 24.04 host에서 실행

기존 TaskCage service, binary, `taskcage` account가 없는 전용 시험 host에서 실행한다. script는 기존
설치를 발견하면 변경하지 않고 종료 코드 77로 중단한다.

```bash
sudo apt-get update
sudo apt-get install -y --no-install-recommends ffmpeg

cargo --version
java -version
ffmpeg -version | head -n 1

bash integration-tests/ffmpeg-reference-workflow.sh
```

script는 다음 순서를 그대로 실행한다.

1. Rust workspace를 build한다.
2. Ubuntu 설치 자산으로 `taskcaged`를 전용 account와 `Delegate=yes` service로 시작한다.
3. 같은 `taskcage` UID에서 live status가 `READY`인지 확인한다.
4. Java SDK가 `/usr/bin/ffmpeg`의 실제 절대 경로를 shell 없이 제출한다.
5. FFmpeg `lavfi` sine input으로 WAVE file을 만들고 `EXITED`, exit code 0, `RIFF`/`WAVE` header를 확인한다.
6. 같은 FFmpeg descendant launcher를 일반 `ProcessBuilder`로 실행해 root-only 종료 뒤 child가 남는 것을
   재현한 후 시험이 해당 child를 명시적으로 정리한다.
7. launcher를 TaskCage에 제출해 wall-time timeout 뒤 `TIMED_OUT`, descendant PID 소멸,
   `cleanup_complete=true`와 task cgroup 수 원상 복구를 확인한다.
8. 임시 Artifact, service, binary, 설정과 시험 account를 제거한다.

성공 출력에는 Ubuntu package의 실제 FFmpeg version, 전체 소요 시간과 다음 문장이 포함된다.

```text
PASS: FFmpeg Local Raw Command 정상 실행, ProcessBuilder descendant 재현, timeout whole-task cleanup을 ...초에 확인했습니다
```

## Java Raw Command 형태

정상 경로는 [FfmpegReferenceWorkflowTest.java](../java-sdk/src/ffmpegE2eTest/java/io/github/taskcage/sdk/FfmpegReferenceWorkflowTest.java)의
다음 계약을 사용한다.

```java
TaskSpec spec = new TaskSpec(
    new ExternalCommand(
        absoluteFfmpegPath,
        List.of(
            "-hide_banner", "-loglevel", "error", "-nostdin", "-y",
            "-f", "lavfi", "-i", "sine=frequency=1000:duration=0.25",
            "-c:a", "pcm_s16le", absoluteOutputPath.toString()),
        absoluteWorkingDirectory,
        Map.of("LANG", "C.UTF-8")),
    explicitResourceBudget);

UUID clientRequestId = UUID.randomUUID();
FinishedTaskSnapshot finished = client.run(clientRequestId, spec, Duration.ofSeconds(15));
```

`run()`은 제출 응답 뒤 유한한 monotonic deadline 안에서 `getTask()`를 polling하고 timeout 시 Task를
자동 취소하지 않는다. 이 wait timeout은 `ResourceBudget.wallTimeLimit`과 별개다. caller-owned
`clientRequestId`는 응답 유실이나 wait timeout 뒤 같은 요청을 다시 식별하기 위한 값이며 daemon restart를
가로지르는 exactly-once 보장은 아니다.

## 비교에서 증명하는 범위

| 경로 | root 종료 뒤 FFmpeg child | 최종 계약 |
|---|---|---|
| 일반 `ProcessBuilder.destroyForcibly()` | 같은 launcher에서 child가 살아 있음을 확인 | 시험이 child를 별도로 정리 |
| TaskCage wall-time timeout | task cgroup 전체 정리 뒤 child PID 없음 | `TIMED_OUT`과 cleanup 완료 뒤 `FINISHED` |

launcher는 시험 안전을 위해 FFmpeg 실행을 최대 30초로 제한하고, 모든 program과 ready file을 절대 경로로
받는다. TaskCage의 submitted command는 shell 문자열, PATH lookup 또는 ambient environment 상속을 사용하지
않는다.

이 한 workflow는 정상 실행과 timeout cleanup을 증명한다. cancel, OOM, PID limit, release artifact,
Maven Central, Execution Profile, Runtime Package, Remote와 성능 benchmark는 후속 독립 작업이다.
