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

이 목표는 순수 Java 라이브러리를 대체하는 것이 아니다. JVM 밖의 신뢰된 CLI를 사용해야 하는 경우에만
Capsule을 적용한다. 첫 도입 경로와 ProcessBuilder 호환성 목표는
[Capsule 도입과 ProcessBuilder 호환성](capsule-adoption.md)을 따른다.

## 실행 모드

두 mode는 동일한 Capsule request와 `ExecutionResult` 의미를 공유한다. 다만 Linux cgroup 제한과
whole-process cleanup은 Linux execution backend가 실제로 검증한 경우에만 보장한다.

| Mode | 목적 | 주요 사용처 |
|---|---|---|
| **ExternalRunner** | 실행 중인 daemon에 UDS 또는 TLS로 Capsule 실행을 요청 | Docker Compose 개발, 팀 공용 runtime, 운영 |
| **EmbeddedRunner** | Java SDK가 private `taskcage-exec` helper를 관리 | daemon 설치 없이 단일 Java Worker 안에 배포 |

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
Artifact staging residue를 daemon 컨테이너 내부에서 검사한다. 관련 변경의 CI는 이 Linux gate를 통과해야 한다.

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

## 도입과 보안 경계

첫 사용자는 Docker Compose 또는 host daemon에 Capsule과 Runtime Package를 import한 뒤 Java SDK로
FFmpeg Capsule을 실행할 수 있어야 한다. Hub와 자동 다운로드는 이 성공 경로의 필수 조건이 아니다.

TaskCage는 신뢰된 CLI의 cgroup 제한·수명주기 runtime이다. untrusted code·archive·script의 보안 격리는
Docker, namespace, seccomp, filesystem/network 정책 같은 별도 계층의 책임이다.

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
