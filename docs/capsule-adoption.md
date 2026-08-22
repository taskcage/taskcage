# Capsule 도입과 ProcessBuilder 호환성

## 목적

TaskCage의 대상은 JVM 안에서 실행되는 순수 Java 라이브러리가 아니라, Java 애플리케이션이 별도 OS
프로세스로 실행하는 신뢰된 외부 CLI다. 이 문서는 Capsule-first 모델을 선택하는 이유, 기존
`ProcessBuilder`·wrapper 사용자의 전환 경로와 호환성 목표를 정의한다.

현재 구현된 wire·실행 계약은 [Capsule 실행 계약](capsule-execution-contract.md)을 따른다. 이 문서의
호환성 목표는 이후 Capsule schema를 확장할 때의 설계 기준이며, 아직 구현되지 않은 기능을 현재
제공되는 API로 주장하지 않는다.

## 왜 외부 CLI인가

PDFBox, Apache POI처럼 JVM 내부에서 실행되는 순수 Java 라이브러리를 사용할 수 있다면 별도 process
lifecycle이 없으므로 보통 더 단순하다. TaskCage는 이를 대체하지 않는다.

하지만 FFmpeg의 codec, LibreOffice의 문서 렌더링, Tesseract의 OCR engine, Chromium의 실제 브라우저
동작처럼 native 도구의 기능·성능·결과 호환성이 필요한 작업도 많다. 이 도구들은 대개 입력 하나를
처리하고 종료하는 CLI로 제공된다.

```text
video.mp4     → ffmpeg      → audio.wav
document.pdf  → OCR         → text.json
source.docx   → LibreOffice → output.pdf
```

상주 server나 pool은 시작 비용을 줄일 수 있지만, 상태·메모리 누수·동시성·재시작과 작업 간 오염을
관리해야 한다. 따라서 미디어·OCR·문서 변환 같은 일회성 작업은 상주 Worker가 메시지를 받고, 작업마다
새 CLI process tree를 만드는 구조가 흔하다.

```text
Queue / Worker
  → ffmpeg 실행 후 종료
  → OCR 실행 후 종료
  → PDF 변환 후 종료
```

## 기존 인프라와 책임 경계

TaskCage는 Queue, Kubernetes, Docker 또는 `tini`를 대체하지 않는다.

```text
Queue / Kubernetes
  → 작업 전달·재시도·배치·배포

Worker
  → 업무 로직과 input/output 흐름

Docker + tini
  → 실행 환경·컨테이너 PID 1 신호 전달·zombie 상태 회수

TaskCage
  → 외부 CLI Task 하나의 제한·실행·관찰·전체 정리·결과 확정
```

`tini`는 컨테이너 종료 시 신호 전달과 종료된 자식의 zombie 상태 회수에 유용하다. 그러나 실행 중인
외부 작업을 식별하거나 작업별 CPU·memory·PID·timeout을 적용하지 않고, timeout 뒤 살아 있는
process tree나 partial output을 정리하지도 않는다. TaskCage와 `tini`는 함께 사용할 수 있다.

TaskCage는 보안 sandbox가 아니다. 신뢰되지 않은 code·archive·script를 안전하게 실행하려면 Docker,
namespace, seccomp, filesystem·network 정책 같은 별도 보안 경계가 필요하다.

## Capsule-first 전환 원칙

Capsule은 단순 binary 묶음이 아니라, 외부 CLI를 어떻게 호출하고 어떤 조건에서 성공으로 확정할지를
선언하는 불변 실행 계약이다.

```text
Capsule
  → input/output schema
  → allowed argv materialization
  → Runtime Package reference
  → resource policy
  → output validation and publish
  → failure and cleanup semantics
```

TaskCage는 기존 `ProcessBuilder` 호출을 수정 없이 가로채는 방식을 기본 전략으로 삼지 않는다. 임의
command를 그대로 실행하면 input/output 의미, 성공 조건, artifact 검증과 재현성을 알 수 없으며,
결국 cgroup을 붙인 command wrapper에 머무르기 때문이다.

대신 기존 호출에 흩어진 실행 지식을 Capsule로 한 번 옮기고, 애플리케이션은 Capsule을 호출한다.

```text
기존
Java wrapper 또는 ProcessBuilder → CLI

전환 후
CapsuleRequest → Capsule 실행 계약 → TaskCage → CLI
```

전환 비용은 Capsule scaffold, argv/output 동등성 test와 공식 Capsule 예제로 낮춘다. 목표는 무수정
호환이 아니라, 기존 CLI 호출을 빠르게 검토·계약화해 같은 Capsule을 여러 Worker와 언어 SDK에서
재사용하게 하는 것이다.

## wrapper 라이브러리와의 연결

TaskCage는 Selenium·Playwright처럼 풍부한 도메인 API를 대체하지 않는다. 이 라이브러리들은 브라우저
조작·DOM·세션을 제공하며, TaskCage의 Job Capsule은 input을 받아 결과를 반환하는 일회성 CLI에
최적화되어 있다.

다만 CLI wrapper의 작성자는 자신의 공개 API를 유지한 채 내부 실행 backend만 TaskCage로 바꿀 수 있다.

```text
기존 wrapper API
  → wrapper adapter
      → CapsuleRequest
          → CapsuleRunner
              → TaskCage
```

wrapper adapter는 wrapper의 의미 있는 요청 객체를 `CapsuleRequest`로 변환하고,
`ExecutionResult`를 기존 결과 type 또는 예외로 변환한다. TaskCage가 도구별 wrapper API를 직접
구현하거나 추종하지 않아도, wrapper는 Capsule의 자원 제한·cleanup·output 검증·구조화 결과를
공유할 수 있다.

상주 WebDriver, LibreOffice process pool처럼 여러 논리 작업이 하나의 process를 공유하는 경우에는
개별 Job을 완전히 격리할 수 없다. 이들은 향후 session/worker 모델의 대상이며, Job Capsule에 억지로
포함하지 않는다.

## ProcessBuilder 호환성 목표

Capsule은 `ProcessBuilder`의 완전한 상위 호환이 아니다. 목표는 FFmpeg·OCR·이미지·문서·PDF 변환처럼
신뢰된 일회성 batch CLI에서 쓰는 `ProcessBuilder` 사용례의 80~90%를 표현하는 안전한 부분집합이다.
이 비율은 실제 Java 프로젝트의 호출 corpus로 검증해야 하는 목표이지, 사전 보장 수치가 아니다.

### 지원 목표

| ProcessBuilder 사용 방식 | Capsule 대응 |
| --- | --- |
| executable + argv 배열 | Capsule이 고정한 executable + argv template |
| 문자열·숫자·boolean·enum option | scalar input |
| 반복 인자 | list input |
| 입력 파일 | read-only input artifact |
| 여러 결과 파일 | named output artifact |
| JSON 설정 파일 | object input을 workspace JSON file로 materialize |
| stdin 파일 | input artifact를 선언적 stdin으로 연결 |
| stdout JSON | structured JSON output |
| stdout/stderr capture | bounded tail 또는 named log artifact |
| working directory | isolated task workspace |
| 환경 변수 | manifest allowlist 안의 env input |
| timeout·exit code 확인 | resource policy + success rule |
| 강제 종료 | Cage 전체 process tree cleanup |

argv는 shell 문자열이 아니라 배열로 선언한다.

```text
ffmpeg
["-i", "${source.path}", "-ar", "${sampleRate}", "${audio.path}"]
```

작은 고정 pipeline은 이후 필요성이 검증되면 shell 없이 2~3개 step의 argv 배열로만 지원할 수 있다.
전체 pipeline은 하나의 Task cgroup 안에 포함되어야 한다.

### 의도적으로 지원하지 않는 것

| ProcessBuilder 사용 방식 | 제외 이유 |
| --- | --- |
| shell 문자열, `sh -c` | quoting·injection·재현성이 약해짐 |
| 요청마다 임의 executable 지정 | Capsule의 검증된 runtime 계약이 무너짐 |
| 임의 host working directory | artifact·workspace 경계가 사라짐 |
| 모든 host environment 상속 | 재현성과 secret 경계가 약해짐 |
| `inheritIO()` | 구조화 로그와 출력 상한에 맞지 않음 |
| 대화형 terminal / PTY | Job Capsule 모델과 맞지 않음 |
| 무제한 pipe·redirect graph | Capsule DSL이 범용 shell이 됨 |
| 실행 중인 임의 PID attach | Task 소유권·cleanup 보장을 할 수 없음 |
| 신뢰되지 않은 code 실행 | 보안 sandbox 범위 밖 |

## Capsule에 적합한 프로세스

```text
매우 적합
  → FFmpeg, ImageMagick, Tesseract CLI, OCRmyPDF,
    Ghostscript, Poppler, Pandoc, GDAL

조건부 적합
  → LibreOffice headless 단발 변환,
    Chromium screenshot/PDF,
    trusted compiler·linter·archive extractor

Job Capsule 부적합
  → Selenium WebDriver 재사용,
    LibreOffice process pool,
    database·cache·HTTP server,
    interactive CLI
```

초기 공식 Capsule은 범용 도구 전체가 아니라 하나의 명확한 capability에 집중한다.

```text
ffmpeg-audio-to-wav
ffmpeg-generate-thumbnail
imagemagick-resize
tesseract-image-to-text
libreoffice-docx-to-pdf
```

## 확정된 실행 구조

실행 core와 transport 구조는 다음과 같이 유지한다.

```text
Capsule
  → 외부 CLI의 실행 계약

taskcage-core
  → cgroup, process tree, artifact, cleanup을 담당하는 공통 실행 엔진

taskcaged
  → TLS / UDS, host policy, Capsule catalog를 제공하는 daemon adapter

taskcage-exec
  → EmbeddedRunner를 위한 private helper
```

Java 사용자는 하나의 Capsule 실행 모델을 본다.

```text
Java Application
  → CapsuleRunner
      ├─ ExternalRunner: daemon에 UDS / TLS 연결
      └─ EmbeddedRunner: private taskcage-exec helper 사용
  → CapsuleRequest
  → ExecutionResult
```

ExternalRunner는 Docker Compose 개발·운영의 기본 경로다. EmbeddedRunner는 daemon 설치 없이 단일 Worker
안에서 실행해야 하는 경우를 위한 선택적 배포 방식이며, Linux cgroup delegation을 실제로 증명할 수
있어야 한다.

공개 SDK는 `CapsuleRequest`, `CapsuleRunner`, `ExecutionResult`를 중심으로 단순화한다. `ProfileRequest`,
`ProfileIdentity`, transport별 request/result와 helper lifecycle은 호환·wire 구현 세부사항으로 유지한다.

## 도입 성공 기준

첫 사용자 성공 경로는 다음과 같이 짧아야 한다.

```text
Docker Compose 또는 host에 taskcaged 기동
→ 공식 Capsule과 Runtime Package import
→ Java SDK 의존성 추가
→ FFmpeg Capsule 실행
→ 검증된 output과 termination result 확인
```

초기 수요 검증은 다음 질문으로 판단한다.

1. 실제로 timeout·잔여 process·partial output 문제를 겪는 FFmpeg/OCR/PDF 팀이 있는가?
2. daemon과 cgroup 권한을 운영 절차에 넣을 수 있는가?
3. 첫 공식 Capsule 하나가 기존 ProcessBuilder 호출을 바꿔 볼 이유가 되는가?
4. Capsule 작성·import 비용보다 cleanup·관측성·재시도 판단의 이득이 큰가?

TaskCage의 목표는 외부 CLI를 가장 빠르게 실행하는 것이 아니라, ProcessBuilder에 가까운 호출 경험으로
실행 계약·자원 제한·cleanup·결과 검증을 반복 가능하게 제공하는 것이다.
