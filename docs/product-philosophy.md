# TaskCage 제품 철학과 용어

## 한 문장

TaskCage는 신뢰된 외부 프로세스를 호출 코드의 부수 효과가 아니라, 재현 가능하고 제한된 실행 계약으로 다루게 하는 Linux-native process runtime이다.

이 문서는 제품 수준의 방향과 공개 용어를 정의한다. 현재 구현된 wire 계약은
[Local Protocol v1 API 명세](api-mvp.md), [Local Profile Core API v2](api-profile-v2.md)와
[Remote Protocol v1](remote-protocol-v1.md)을 따른다. Remote는 TLS 1.3, service-account 인증,
Profile authorization과 관리되는 Artifact 전송을 사용하는 별도 opt-in 경로이며 Local framing을 그대로
network에 노출하지 않는다.

> **전환 상태:** 현재 공개 릴리스에는 Local Raw Command와 Local Profile이 함께 존재한다. 다음
> Bundle-first 공개 계약의 목표는 일반 실행을 Bundle/Profile로 한정하고 Raw Command를 공개 API에서
> 제거하는 것이다. 이 문서는 그 목표 모델과 현재 구현의 경계를 함께 기록한다.

## 우리가 믿는 것

### 1. 외부 프로세스는 단순한 child process가 아니다

Chromium, FFmpeg, LibreOffice, OCR, compiler는 애플리케이션 안에서 한 줄로 시작할 수 있지만 실제로는
CPU·메모리·PID·파일·시간·자식 프로세스 트리를 가진 독립 실행 단위다. 따라서 외부 프로세스는 루트
PID 하나가 아니라 **Task**로 다뤄야 한다.

> Task는 특정 실행 계약을 입력과 자원 정책으로 수행하는 일회성 작업이며, cgroup v2 실행 경계·프로세스 트리·상태·결과를 포함한다.

### 2. Task가 자원과 수명주기 경계를 소유한다

공개 계약에서 Task 하나는 task cgroup root 하나를 소유하고, 하나의 task cgroup root는 하나의 Task에만
속한다. 데몬이 관리 목적으로 그 아래에 하위 cgroup을 둘 수 있지만 이는 내부 구현 세부사항이다. 별도
공개 실행 경계 type, API 또는 lifecycle state를 만들지 않는다.

제품 계약은 다음 invariant를 목표로 한다.

- 제한을 적용하고 read-back으로 확인한 뒤에만 외부 프로세스를 시작한다.
- CPU·메모리·PID·벽시계 시간을 Task 단위로 관리한다.
- daemon이 시작한 target과 자식 프로세스를 해당 Task의 cgroup root 아래에서 추적한다.
- timeout·취소·오류 시 루트 PID가 아니라 해당 Task의 프로세스 트리 전체를 정리한다.
- 다른 Task의 cgroup을 종료하거나 제거하지 않도록 소유권과 대상을 확인한다.
- 정리 완료를 확인한 뒤에만 최종 결과를 공개한다.

현재 Local 기준선은 Task별 CPU·메모리·PID 상한과 whole-task cleanup을 제공하지만 Task별 자원 총량을
미리 예약하거나 성능을 완전히 격리하지 않는다. 또한 target은 daemon과 같은 credential로 실행되므로,
악의적인 same-UID target이 쓰기 가능한 cgroup filesystem을 이용해 sibling task cgroup으로 이동하는
것까지 막는 permission boundary는 아직 없다. 현재의 신뢰된 외부 프로세스 전제는 이 제한을 포함한다.

따라서 “Task 1이 Task 2에 어떤 영향도 주지 않는다”는 현재 보장이 아니다. 그 수준의 자원·수명주기
경계를 공개 보장하려면 target privilege 분리, task cgroup ownership·permission, 교차 이동 방지,
aggregate admission policy와 실제 Linux adversarial test를 먼저 설계해야 한다. 그 뒤에도 cgroup v2만으로
공유 filesystem·network·syscall·kernel global resource까지 격리되지는 않는다. TaskCage는
VM·container·security sandbox가 아니며, 신뢰할 수 없는 code의 보안 격리는 별도 계층의 책임이다.

### 3. 실행은 명령어가 아니라 계약이어야 한다

실행 파일 경로와 shell 문자열은 환경에 묶여 있고 검증·공유·재현하기 어렵다. TaskCage는 제품
계약에서 실행을 **Execution Profile**로 표현한다.

Execution Profile은 어떤 도구를 어떤 입력으로, 어떤 자원 정책 아래, 어떤 Runtime Package로, 어떤
결과물로 실행하는지를 정의한 버전 관리 선언이다. daemon은 이 계약을 최종 검증하고 shell을 거치지
않는 argv와 Task의 cgroup 경계를 구성한다.

일반 사용자는 타입 안전한 Profile Binding 또는 범용 `ProfileRequest`로 의미 있는 작업을 호출한다.
Bundle 제작자는 Custom Profile을 만들어 배포할 수 있다. 다음 Bundle-first 공개 계약에는 실행 파일과
argv를 직접 지정하는 Raw Command를 포함하지 않는다. 현재 Local Raw Command는 기존 공개 릴리스의
호환 경로이며, 제한을 우회하는 경로는 아니다.

### 4. 재현성과 이식성은 선언해야 얻어진다

cgroup은 실행을 제한하지만 실행 파일·라이브러리·codec·font·환경 변수·CPU 아키텍처까지 같게 만들지는
않는다. TaskCage는 어디서나 모든 프로그램을 실행한다고 약속하지 않는다. 지원 Linux 플랫폼 위에서
Execution Profile, digest로 고정한 Runtime Package, 플랫폼 호환성, Artifact 계약과 검증된 runtime을
명시해 재현성을 만든다.

Docker가 애플리케이션 전체를 Image로 배포한다면, TaskCage는 외부 프로세스 작업을 Profile과 Package
참조로 구성된 실행 계약으로 만든다.

```text
TaskCage Bundle
├─ Execution Profile
├─ Runtime Package ref + digest
├─ Platform requirements
├─ Resource policy
└─ Bundle signature

Runtime Package
├─ Executable
├─ Libraries/codecs/fonts
├─ Platform manifest
├─ SBOM/licenses
└─ Package digest/signature
```

Bundle은 Profile, Package digest, platform, policy와 서명을 가진 불변 실행 계약이다. 초기 배포물은
사람이 검토 가능한 `.tcbundle.tar.gz` archive로 만들 수 있으며, Package payload를 포함하는 배포 archive도
허용할 수 있다. 다만 실행 계약은 항상 Package digest를 참조하며, daemon cache에서는 Package를 별도
entry로 관리해 여러 Bundle이 같은 digest를 공유한다. Bundle과 Package의 무결성 검증이 끝나기 전에는
Task를 시작하지 않는다. archive와 manifest의 상세 형식은 [Bundle 형식 초안](bundle-format.md)을 따른다.

### 5. 안전은 제한 없는 fallback보다 중요하다

cgroup controller, 권한, 원자적 task cgroup entry 또는 제한값 read-back을 확인할 수 없다면 제한 없는
상태로 실행하지 않는다. whole-task cleanup을 증명할 수 없다면 새 Task를 시작하지 않고, 필요하면
fail-stop과 시작 복구를 선택한다.

### 6. Bundle과 Binding은 확장 가능한 생태계다

Bundle은 공식 도구 목록이 아니라 외부 제작자도 만들 수 있는 배포 단위다. Bundle 제작자는 Runtime
Package와 Execution Profile을 만들고, manifest·digest·서명으로 하나의 실행 계약을 배포한다. Binding
제작자는 그 Profile input/output schema를 Java 등의 타입 안전한 도메인 API로 매핑해 각 언어의 package
registry에 독립 배포할 수 있다.

Bundle과 Binding은 권장되는 조합이지만 강하게 결합하지 않는다.

```text
Bundle only
  → Generic ProfileRequest로 실행

Bundle + Java Binding
  → Java domain API로 실행
```

Binding은 Bundle의 신뢰를 부여하지 않는다. daemon은 Binding이 보낸 요청도 Bundle 서명, allowlist,
Profile schema, Artifact와 정책을 최종 검증한다. Binding은 지원하는 Bundle/Profile, Core SDK와 Protocol
버전 범위를 공개해야 한다.

### 7. Local Core 계약을 유지하고 Remote는 제한된 별도 경로로 제공한다

Core SDK는 장기적으로 transport와 상관없이 같은 Task·결과·종료 원인 계약을 제공한다.

```text
TaskCage Core SDK
├─ Current Local Transport (UDS): Raw Command와 Profile
├─ Target Local Transport (UDS): 승인된 Bundle/Profile
└─ Remote Transport (TLS): 승인된 Profile과 관리되는 Artifact
```

현재 공개 기준선은 Local UDS의 Raw Command·Profile과 인증된 Remote Profile 실행이다. Local과 Remote는
같은 Task 결과·종료 원인·정리 계약을 유지하지만 transport와 허용된 실행 입력은 명시적으로 구분한다.
다음 Bundle-first 단계는 Local Bundle import, Runtime Package 검증, 범용 Profile API와 첫 Binding을
하나의 완결된 사용자 흐름으로 검증한다. 중앙 Hub는 그 뒤 실제 공유·배포 요구가 확인된 경우에만 검토한다.

Remote는 Local Protocol v1 framing을 network에 그대로 노출하지 않는다. 원격 Profile 실행의 TLS,
service-account authentication, authorization, Artifact reference와 failure contract는
[Remote Protocol v1](remote-protocol-v1.md)에서 별도로 정의한다. 이 계약은 Remote Raw Command와
Local UDS fallback을 허용하지 않는다.

### 8. 기존 인프라를 대체하지 않고 연결한다

TaskCage는 Kafka, Kubernetes, Docker 또는 Temporal의 대체재가 아니다.

- Kafka와 Queue는 작업 전달, 업무 재시도와 소비자 확장을 담당한다.
- Kubernetes와 Docker는 애플리케이션 배포와 환경 격리를 담당한다.
- TaskCage는 신뢰된 외부 프로세스 작업의 실행, 자원 제한, 관찰과 전체 정리를 담당한다.

애플리케이션은 Core SDK로 직접 호출할 수 있고, 별도 Worker가 표준 Task 계약을 소비할 수도 있다.
메시지 영속화·분산 scheduling·node autoscaling은 TaskCage daemon의 책임이 아니다.

## 제품 경계

TaskCage는 다음을 제공하는 것을 목표로 한다.

- 외부 프로세스의 Task 단위 추상화
- Linux cgroup v2 기반 자원·수명주기 관리
- Bundle/Profile 기반 실행 계약
- Local UDS와 인증된 Remote runtime 연결
- 일관된 결과, Artifact와 종료 원인

TaskCage는 다음을 기본 제공하지 않는다.

- 신뢰할 수 없는 코드의 완전한 보안 격리
- 메시지 영속화와 업무 재시도 workflow
- 분산 scheduler와 node autoscaling
- 임의 URL에서 임의 binary를 받아 실행하는 기능
- 중앙 Hub server 의존성

Hub는 Bundle·Profile·Binding metadata를 배포할 수 있는 장기 후보일 뿐 Local Public Alpha나 Local
Product Alpha 구성요소가 아니다. 제품은 Hub 없이 설치·검증된 계약으로 동작해야 하며, 이 저장소는 현재
Hub server를 구현하거나 운영하지 않는다.

## 표준 용어

| 용어 | 정의 |
|---|---|
| TaskCage | 신뢰된 외부 프로세스를 제한된 실행 계약으로 다루는 Linux-native process runtime |
| Task | 특정 실행 계약을 입력과 자원 정책으로 수행하는 일회성 작업이며, cgroup v2 실행 경계·프로세스 트리·상태·결과를 포함하는 공개 실행 단위 |
| TaskCage Daemon (`taskcaged`) | Task를 검증하고 task cgroup을 생성해 외부 프로세스를 실행·관찰·정리하는 runtime |
| TaskCage Core SDK | 현재는 Local Raw Command·Profile과 인증된 Remote Profile 연결을 제공하며, 목표 공개 계약에서는 Bundle/Profile Task 제출·조회·취소를 제공하는 공통 SDK |
| Execution Profile | 입력·출력 schema, argv 구성 규칙, Runtime Package 참조와 기본 자원 정책을 정의한 버전 관리 실행 계약 |
| TaskCage Bundle | Execution Profile, Runtime Package ref + digest, 호환성·정책·무결성 정보를 담은 불변 실행 계약이자 배포 단위 |
| Runtime Package | 실행 binary와 필요한 library·codec·font·설정을 묶어 별도로 cache하는 플랫폼별 실행물 |
| Profile Binding | Execution Profile을 Java 등의 타입 안전한 domain API로 매핑해 독립 배포하는 언어별 편의 library |
| Artifact | Task가 입력으로 사용하거나 결과로 생성하는 file·URI·data 참조 |
| Raw Command | 현재 공개 Local 릴리스의 legacy 호환 API. 다음 Bundle-first 공개 계약에는 포함하지 않음 |
| TaskCage Hub | Bundle·Profile·Binding metadata를 저장·검색·검증·배포할 수 있는 장기 Registry 후보. Local Public Alpha와 Local Product Alpha에는 포함하지 않음 |

**TaskCage는 프로세스를 실행하는 도구가 아니라, 외부 프로세스를 신뢰 가능한 작업 계약으로 바꾸는 runtime이다.**
