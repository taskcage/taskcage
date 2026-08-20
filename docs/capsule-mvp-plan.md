# Capsule-first MVP 계획

## 목표

TaskCage의 첫 검증 목표는 Java 개발자가 외부 CLI를 `ProcessBuilder` 대신 **Capsule 실행 계약**으로
호출하고, 제한·정리·결과를 일관되게 받는 경험을 만드는 것이다.

```text
Java Application / Worker
        ↓ Java SDK
TaskCage Daemon
        ↓ taskcage-core
Capsule → external process
```

Capsule은 실행 파일 묶음이 아니다. 하나의 Capsule은 다음 실행 지식을 함께 고정한다.

```text
typed input/output schema
allowed argv materialization
Runtime Package reference
CPU · memory · PID · timeout policy
output validation and artifact publish
failure and cleanup result
```

첫 사용자 성공 기준은 다음과 같다.

> 일반적인 Java Worker가 executable path나 shell 문자열을 몰라도 FFmpeg Capsule 하나를 실행하고,
> 실패 뒤에도 process tree와 partial artifact가 깨끗하게 정리되는가?

## 현재 기준선과 목표 상태

현재 `main`은 daemon-backed Local Profile, Capsule archive import, Runtime Package 검증, FFmpeg
Profile, Local UDS와 opt-in Remote TLS E2E를 제공한다. 개발 Compose의 기본 daemon은 Local UDS를
사용하고, 권위 있는 External Capsule MVP 검증은 Remote TLS profile에서 수행한다.

MVP의 다음 변경은 이를 숨기지 않는다. **개발자가 권장 경로 하나만 따라 실제 Capsule을 실행할 수 있게
Compose 기반 ExternalRunner 경험을 완성하는 것**이 우선이다. EmbeddedRunner는 같은 계약을 공유하는
선택적 배포 방식이며, 첫 사용자 경로를 막지 않는다.

## 실행 모드

두 mode는 동일한 Capsule request와 `ExecutionResult` 의미를 공유한다. 다만 Linux cgroup 제한과
whole-process cleanup은 Linux execution backend가 실제로 검증한 경우에만 보장한다.

| Mode | 목적 | 주요 사용처 | 현재 상태 |
|---|---|---|---|
| **ExternalRunner** | 실행 중인 daemon에 UDS 또는 TLS로 Capsule 실행을 요청 | Docker Compose 개발, 팀 공용 runtime, 운영 | Compose TLS FFmpeg Capsule 경로와 반복 CI gate 구현. green Linux 실행 증거 필요 |
| **EmbeddedRunner** | Java SDK가 private `taskcage-exec` helper를 관리 | daemon 설치 없이 단일 Java Worker 안에 배포 | 후속 선택적 확장 |

## 로컬 개발의 기본 경험

MVP의 권장 로컬 경로는 Embedded가 아니라 Docker Compose 기반 daemon이다.

```text
docker compose --profile remote-test up -d remote-taskcaged
        ↓ TLS
Java application / integration test
        ↓ Capsule request
taskcaged → taskcage-core → external process
```

이 경로가 제공해야 할 경험은 다음과 같다.

- 개발자는 Linux cgroup delegation이나 helper artifact를 직접 준비하지 않는다.
- 팀은 동일한 daemon, Capsule과 Runtime Package 환경을 재현한다.
- Java SDK의 실제 TLS 연결과 Capsule 결과를 로컬에서 검증한다.
- macOS/Windows 개발자도 Docker Desktop Linux VM 안에서 Linux cgroup 동작을 확인한다.
- 운영의 ExternalRunner와 같은 Capsule request/result 모델을 사용한다.

Compose는 개발 전용 CA와 daemon 서버 인증서를 제공한다. SDK는 그 CA를 명시적으로 신뢰하고 hostname
검증을 유지한다. 개발 편의를 이유로 운영 기본값에서 hostname 검증을 끄거나 모든 인증서를 신뢰하지
않는다.

`bash dev/container/run-remote-e2e.sh`는 깨끗한 Compose volume을 만들고 Remote TLS daemon과 Java
ExternalRunner를 연결한다. 정상 실행, timeout, cancel, memory/PID limit을 실행한 뒤 job cgroup과
Artifact staging residue를 daemon 컨테이너 내부에서 검사한다. 관련 변경의 일반 CI도 이 명령을 실행한다.
MVP 완료 판정에는 해당 Linux gate의 실제 green 결과가 필요하다.

## 구현 순서

### 1. External Capsule contract 완결

- Capsule identity, Profile, typed input/output, Artifact와 `ExecutionResult`를 ExternalRunner 경계에서
  일관되게 연결한다.
- 성공은 `exitCode == 0`만이 아니라 output validation, atomic publish, whole-task cleanup 확인까지
  포함한다.
- FFmpeg Capsule 하나를 기준 사례로 고정한다.

### 2. Compose TLS developer runtime

- 개발용 TLS daemon, CA·서버 인증서와 Capsule/Runtime Package import 초기화를 제공한다.
- Java sample과 integration test는 explicit trust material로 daemon DNS에 연결한다.
- TLS handshake, 인증 실패와 hostname 검증 실패를 E2E로 확인한다.

### 3. Java ExternalRunner 사용자 경험

- endpoint와 trust material만으로 daemon에 연결하는 최소 설정을 제공한다.
- 사용자는 executable path, argv, cgroup setting, staging output path를 직접 지정하지 않는다.
- FFmpeg request/result는 Java 객체 API로 제공하되, Core SDK의 generic Capsule request를 우회하지 않는다.

### 4. Capsule lifecycle 검증

- 정상 실행, timeout, cancel, memory/PID limit을 Compose Linux 환경에서 반복 검증한다.
- 모든 terminal result에 termination reason, stdout/stderr tail, resource usage와 Artifact 결과를 포함한다.
- descendant process, task cgroup, staging artifact가 남지 않음을 확인한다.

### 5. EmbeddedRunner (선택적 확장)

- `taskcage-exec` private helper의 cancel, close, crash cleanup과 platform artifact를 완성한다.
- 동일 Capsule에 대해 ExternalRunner와 result·failure·Artifact·cleanup 의미를 conformance test로 비교한다.
- Java Worker container에서 cgroup delegation과 종료 뒤 잔여 process 정리를 검증한다.

## MVP 완료 조건

다음 흐름이 깨끗한 Docker Compose Linux 환경에서 반복 가능하면 External Capsule MVP를 완료로 본다.

```text
docker compose up
→ Java SDK TLS connection
→ ffmpeg-audio-to-wav Capsule execution
→ validated output Artifact
→ timeout / cancel / memory / PID limit
→ whole-task cleanup confirmation
```

EmbeddedRunner는 같은 Capsule contract를 유지해야 하지만, 위 ExternalRunner 경로를 먼저 완결하는 데
필수 조건은 아니다.

## MVP에서 보류하는 것

- 중앙 Hub와 자동 Capsule 다운로드
- 여러 언어 SDK 동시 지원
- Worker/MQ adapter, distributed scheduler와 autoscaling
- 복잡한 code generation과 범용 stdout parser
- 다중 output orchestration
- 새 public Raw Command API
- 보안 sandbox, namespace, seccomp, filesystem/network isolation

성능 수치는 보조 지표다. 핵심은 ProcessBuilder 대비 호출 경험을 과도하게 바꾸지 않으면서, Capsule이
정의한 제한·정리·결과 계약을 반복해서 보장하는 것이다.
