# Capsule-first MVP 계획

## 목표

TaskCage의 핵심 제품은 외부 프로그램을 직접 호출하는 API가 아니라, **재현 가능한 실행 계약인
Capsule**이다. Capsule은 실행 파일만 담는 패키지가 아니라 typed input/output, 안전한 argv 구성,
자원 정책, timeout·cancel, 결과 검증과 전체 프로세스 정리를 함께 정의한다.

첫 사용자 성공 기준은 다음과 같다.

> 일반적인 Java Worker가 실행 파일 경로와 shell 문자열을 몰라도 하나의 Capsule을 실행하고,
> 실패 뒤에도 프로세스와 결과물이 깨끗하게 정리되는가?

## 현재 기준선

현재 `main`에는 다음 daemon-backed 기능이 있다.

- Linux cgroup v2 Task 실행·제한·관찰·전체 정리
- Runtime Package import/cache와 서명된 local archive catalog
- 설치된 Profile을 통한 typed input/output 실행
- Local UDS Core SDK와 Java `ProfileRequest`
- timeout·cancel·Artifact publish·멱등성·종료 결과

이 구현의 archive와 내부 Rust 모듈은 아직 `Bundle` 명칭을 사용한다. 따라서 이번 전환은 기능을
다시 만드는 작업이 아니라, 사용자 개념을 Capsule로 통일하고 실행 backend를 교체 가능한 구조로
정리하는 작업이다.

## 단계별 처리 순서

### 0. 용어와 경계 정리

- 공개 문서와 새 Java API에서 `Capsule`을 기본 용어로 사용한다.
- `Bundle`은 마이그레이션 기간에만 기존 archive·schema의 호환 명칭으로 남긴다.
- 새 문서에서 Hub, 자동 다운로드, 분산 scheduler, Worker adapter를 MVP 필수 구성요소로 표현하지 않는다.
- 기존 Raw Command는 호환 경로로만 문서화하고 새 Capsule 흐름의 기본 진입점으로 사용하지 않는다.
- archive 확장자와 schema를 실제 코드와 함께 바꾸는 breaking migration은 별도 변경으로 진행한다.

### 1. 공통 Capsule 실행 계약

언어와 backend가 달라도 다음 의미는 같아야 한다.

- Capsule identity와 version
- typed input/output schema
- 선언된 argv materialization 규칙
- Runtime Package 참조와 플랫폼 조건
- resource policy와 허용 override
- timeout·cancel·whole-task cleanup
- output validation과 atomic publish
- 종료 원인·사용량·정리 완료가 포함된 결과

Java SDK는 이 계약을 Java 값 객체와 `CapsuleRunner` 인터페이스로 표현하고, daemon은 최종 검증자다.

### 2. EmbeddedRunner 우선

첫 제품 경험은 별도 daemon 설치 없이 실행하는 EmbeddedRunner다.

```java
try (CapsuleRunner runner = CapsuleRunner.embedded(capsule)) {
    ExecutionResult result = runner.execute(request);
}
```

Embedded backend는 Java가 cgroup semantics를 다시 구현하지 않도록, 검증된 Rust execution core를
private helper/library 형태로 호출하는 것을 우선 검토한다. Java가 cgroup 파일을 직접 조작하는
구현은 권한·race·cleanup 의미가 중복되므로 MVP의 기본 선택으로 삼지 않는다.

EmbeddedRunner가 제공해야 하는 최소 기능은 하나의 Runtime Package, 하나의 Execution Profile,
typed request/result, resource policy, timeout·cancel, whole-process cleanup이다.

### 3. ExternalRunner 연결

EmbeddedRunner와 동일한 `CapsuleRunner` 계약에 현재 daemon-backed Local UDS를 연결한다.

- 사용자는 backend를 바꿔도 request/result 의미를 다시 배우지 않는다.
- ExternalRunner는 운영 환경에서 daemon의 host 정책과 cgroup 관찰성을 활용한다.
- Remote TLS는 인증·Artifact·보존 계약이 필요한 별도 단계이며 Embedded MVP의 선행 조건이 아니다.

### 4. 첫 공식 Capsule

FFmpeg 전체 CLI가 아니라 `ffmpeg-audio-to-wav@1.0.0` 하나를 기준으로 검증한다.

- 입력: local Artifact, sample rate, mono/stereo
- 출력: 검증된 `audio/wav` Artifact
- 실행: Capsule이 고정한 argv와 Runtime Package entrypoint
- 실패: non-zero, timeout, cancel, output validation 실패를 구분된 결과로 반환

### 5. 확장과 배포

한 Capsule의 실행과 사용자 경험이 확인된 뒤에만 다음을 추가한다.

- Capsule archive의 새 확장자·schema와 legacy Bundle read-only migration
- Java typed convenience API와 필요할 때의 generated mapper
- 여러 Runtime Package와 플랫폼별 artifact
- GitHub Release 또는 조직 artifact 저장소 기반 파일 배포

Hub는 여러 조직·호스트가 Capsule 검색과 자동 설치를 실제로 요구할 때 시작한다. 초기 MVP는
Hub 없이 local import와 명시적 Package digest만으로 동작해야 한다.

## MVP에서 보류하는 것

- 중앙 Hub와 자동 Runtime Package 다운로드
- 여러 언어 SDK 동시 지원
- code generation과 복잡한 stdout parser
- 다중 output orchestration
- Kafka/Queue Worker 제품과 분산 scheduler
- Kubernetes Operator와 자동 scale-out
- 보안 sandbox, namespace, seccomp, filesystem/network 정책
- 새 public Raw Command API

## 완료 기준

다음 흐름이 깨끗한 Linux/container 개발 환경에서 반복 가능하면 Capsule MVP를 완료로 본다.

```text
Capsule 정의
→ Runtime Package 검증
→ EmbeddedRunner 실행
→ ExternalRunner 동일 요청 실행
→ timeout/cancel 및 자식·손자 정리 확인
→ output validation·artifact publish 확인
→ Java 예제로 결과와 종료 원인 확인
```

성능 수치는 보조 지표다. 핵심은 ProcessBuilder 대비 설치·호출 경험을 과도하게 바꾸지 않으면서,
제한·정리·결과 계약을 반복해서 보장하는 것이다.
