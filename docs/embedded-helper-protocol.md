# `taskcage-exec` private helper protocol

`taskcage-exec`는 `taskcaged`를 대체하는 공개 daemon이 아니다. Java `EmbeddedRunner`가 자식
프로세스로 lazy-start하는 Linux 전용 helper이며, stdin/stdout의 newline-delimited JSON만 사용한다.
helper는 공개 socket, systemd service, 인증 endpoint를 만들지 않는다.

## Lifecycle

```text
EmbeddedRunner 시작
  → taskcage-exec 시작
  → getCapabilities
  → execute 여러 번
  → shutdown
```

helper가 EOF를 받으면 현재 요청을 정리한 뒤 종료한다. malformed frame이나 실행 오류는 해당 요청의
`error` 응답으로 반환하며, helper가 예기치 않게 종료되면 SDK는 이를 backend failure로 변환한다.

## Frames

모든 frame은 한 줄의 UTF-8 JSON이며 최대 1 MiB다. 요청과 응답에는 caller가 생성한 `requestId`가
포함된다.

### Capabilities

```json
{"type":"getCapabilities","requestId":"cap-1"}
```

```json
{"type":"capabilities","requestId":"cap-1","payload":{"helperVersion":"0.1.0","protocolVersion":1,"maxFrameBytes":1048576}}
```

### Execute

`execute`의 `program`과 `args`는 shell 문자열이 아닌 argv token이다. helper는 요청마다 cgroup을 만들고
제한을 read-back한 뒤 실행한다. timeout 또는 오류가 나면 process tree와 cgroup을 정리한 뒤에만
terminal response를 보낸다.

```json
{
  "type":"execute",
  "requestId":"run-1",
  "payload":{
    "program":"/usr/bin/ffmpeg",
    "args":["-version"],
    "workingDirectory":"/tmp",
    "environment":{},
    "limits":{
      "cpuMax":{"quotaMicros":100000,"periodMicros":100000},
      "memoryMaxBytes":134217728,
      "pidsMax":32,
      "wallTimeLimitMs":30000
    },
    "output":{"stdoutTailMaxBytes":65536,"stderrTailMaxBytes":65536}
  }
}
```

정상·timeout 결과는 `finished`로 반환한다. `outcome`은 현재 `SUCCEEDED` 또는 `TIMED_OUT`이며,
프로그램 exit code, signal, 출력 tail, CPU 시간, memory peak를 포함한다.

### Shutdown

```json
{"type":"shutdown","requestId":"close-1"}
```

```json
{"type":"shutdownAck","requestId":"close-1"}
```

## 범위

현재 helper protocol은 core execution seam을 검증하기 위한 private command envelope다. Capsule/Profile
identity와 typed input/output 해석은 Java `EmbeddedRunner`와 공통 Capsule contract를 연결하는 다음
단계에서 추가한다. 이 private protocol을 외부 호환 API로 사용해서는 안 된다.
