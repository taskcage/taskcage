# TaskCage

**TaskCage는 신뢰된 외부 CLI 호출을 제한되고 정리가 확인되는 재현 가능한 Capsule 실행으로 바꾸는 Linux-native runtime이다.**

Java Worker가 FFmpeg, OCR, PDF 변환기, 이미지 도구처럼 무거운 외부 프로그램을 호출할 때, TaskCage는 작업 하나를 Linux cgroup v2 경계에서 실행하고 제한·관찰·정리한다.

> **Public Alpha:** 현재 설치 가능한 최신 버전은 daemon `0.5.0`, Java SDK `0.4.0`이다. `0.x`에서는 공개 API와 운영 계약이 minor 버전에서 변경될 수 있다.

## 왜 TaskCage인가?

`ProcessBuilder`로 외부 프로그램을 시작하는 것은 쉽다. 하지만 운영 코드에는 곧 다음 책임이 따라온다.

```text
argv 구성 → timeout → stdout/stderr 수집 → 자식 프로세스 정리
→ CPU·memory·PID 제한 → 부분 결과 삭제 → 실패 원인 판별
```

한 Worker 안에서 작업이 겹치면 한 번의 FFmpeg·OCR·Chromium 실행이 서버의 정상 요청까지 영향을 줄 수 있다. timeout 뒤 루트 PID만 종료해서는 손자 프로세스나 부분 결과가 남을 수도 있다.

TaskCage는 이 실행 하나를 **Task**로 만들고, Task가 어떤 프로그램을 어떤 입력과 성공 조건으로 실행하는지는 **Capsule**에 선언한다.

```text
Java application / Worker
        │ Capsule request
        ▼
TaskCage Java SDK
        ▼
taskcaged
        ▼
Task cgroup ──► FFmpeg / OCR / PDF tool / compiler
```

## 첫 번째 Capsule: FFmpeg

Capsule은 단순한 실행 파일 묶음이 아니다. `ffmpeg-audio-to-wav@1.0.0` Capsule은 FFmpeg Runtime Package, 입력·출력 schema, 허용된 argv 구성, 자원 정책, 결과 검증을 함께 고정한다.

Java 애플리케이션은 실행 파일 경로나 shell 문자열을 직접 전달하지 않는다.

```java
CapsuleRequest request = CapsuleRequest.builder("ffmpeg-audio-to-wav", "1.0.0")
    .artifact("source", source)
    .int64("sample_rate_hz", 16_000)
    .int64("channels", 1)
    .build();

try (CapsuleRunner runner = CapsuleRunner.external(taskCageClient)) {
    CapsuleExecutionResult result = runner.execute(request, Duration.ofMinutes(2));
}
```

이 요청은 설치된 Capsule과 Runtime Package를 daemon이 검증한 뒤 실행한다. 결과에는 종료 이유, 제한된 stdout/stderr tail, 자원 사용량과 검증·publish된 output Artifact가 포함된다.

## 개발 환경에서 FFmpeg Capsule 확인하기

가장 빠른 검증 경로는 개발 전용 Docker Compose 환경이다. Linux cgroup v2 daemon, FFmpeg Runtime Package, 서명된 개발용 Capsule을 준비하고 Java FFmpeg 예제를 실행한 뒤 정리한다.

```bash
bash dev/container/run-ffmpeg-example.sh
```

실제 TLS 연결, artifact upload/download, 정상 실행·timeout·cancel·memory/PID 제한과 cleanup까지 포함한 검증은 다음 명령으로 실행한다.

```bash
bash dev/container/run-remote-e2e.sh
```

이 Compose 구성은 **개발·E2E 전용**이다. cgroup lifecycle 검증을 위해 daemon에 높은 권한을 주므로 신뢰할 수 있는 개발 장비에서만 실행해야 한다. 상세 조건과 권한 경계는 [컨테이너 기반 로컬 개발 환경](dev/container/README.md)을 참고한다.

## TaskCage가 보장하는 것

- 외부 프로세스를 시작하기 전에 Task cgroup과 CPU·memory·PID·벽시계 시간 제한을 적용하고 확인한다.
- timeout·취소·오류 시 루트 PID가 아니라 해당 Task가 만든 프로세스 트리 전체를 정리한다.
- cleanup 완료를 확인한 뒤에만 최종 결과를 공개한다.
- exit code만으로 추측하지 않고 cgroup 이벤트와 프로세스 상태를 함께 사용해 종료 이유와 사용량을 반환한다.
- Capsule의 signature, Runtime Package digest, 플랫폼 조건, 입력 schema와 허용된 정책을 실행 전에 검증한다.

TaskCage는 제한을 확인할 수 없으면 제한 없는 실행으로 fallback하지 않는다.

## TaskCage가 하지 않는 것

TaskCage는 보안 sandbox나 컨테이너 대체재가 아니다. 신뢰할 수 없는 코드를 격리하지 않으며, 파일시스템, 네트워크, syscall, 사용자 권한의 보안 경계는 컨테이너나 별도 정책의 책임이다.

또한 Queue·Kafka는 작업 전달과 재시도를, Docker·Kubernetes는 애플리케이션 배포와 환경 격리를 담당한다. TaskCage는 그 안에서 **외부 CLI 작업 하나의 실행·제한·정리**를 담당한다.

| 도구 | 주로 해결하는 문제 |
| --- | --- |
| `ProcessBuilder` | 애플리케이션에서 로컬 프로세스 시작 |
| Docker / Kubernetes | 실행 환경 패키징, 격리, 배포와 배치 |
| Queue / Worker | 작업 전달, 재시도, 처리량 제어 |
| **TaskCage** | Capsule 계약에 따른 CLI 실행, cgroup 제한, 프로세스 트리 정리, 결과 확인 |

Docker가 프로그램의 실행 환경을 재현 가능하게 만든다면, TaskCage는 외부 프로그램을 **호출하는 방법과 성공 조건**을 Capsule로 재현 가능하게 만든다. 두 도구는 함께 사용할 수 있다.

## 현재 범위

- Linux cgroup v2, x86-64 또는 ARM64
- Java 17+ SDK
- Local UDS 및 인증된 opt-in Remote TLS Capsule 실행
- Capsule archive import와 catalog 기반 Profile 실행
- FFmpeg reference Capsule과 Java E2E

현재 공개 릴리스에는 기존 사용자를 위한 Local Raw Command·Profile API도 남아 있다. 새 integration의 권장 경로는 Capsule import 후, Capsule이 선언한 typed input을 실행하는 방식이다. Hub, 자동 Capsule 다운로드, 분산 scheduler와 여러 언어 SDK는 현재의 필수 구성요소가 아니다.

## 설치와 사용

- [Ubuntu daemon 설치](docs/install-ubuntu.md)
- [Java SDK](java-sdk/README.md) — Maven Central 좌표와 Local/Remote 사용법
- [Java FFmpeg 예제](examples/ffmpeg-java/README.md)
- [FFmpeg Audio-to-WAV Capsule](docs/ffmpeg-capsule.md)
- [Capsule 실행 계약](docs/capsule-execution-contract.md)
- [Capsule archive 형식](docs/bundle-format.md)
- [제품 철학과 용어](docs/product-philosophy.md)

## 검증과 기여

소스에서 기본 품질 검사를 실행하려면:

```bash
cargo fmt --check
cargo clippy -- -D warnings
cargo test

cd java-sdk
./gradlew test
```

실제 Linux cgroup 검증 방법은 [Linux 통합 시험](integration-tests/README.md)을, 기여 절차는 [CONTRIBUTING.md](CONTRIBUTING.md)를 참고한다. 버그와 제안은 [GitHub Issues](https://github.com/taskcage/taskcage/issues)에 남겨 주세요.
