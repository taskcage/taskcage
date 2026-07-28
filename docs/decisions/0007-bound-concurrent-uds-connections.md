# 0007. UDS 동시 연결 수를 명시적으로 제한한다

## 문제

`taskcaged`는 서로 다른 Unix domain socket 연결을 동시에 처리하지만 연결 handler 수에 상한이 없었다.
요청을 보내지 않거나 frame prefix 일부만 보내는 클라이언트가 연결을 계속 만들면 작업 실행 한도와
무관하게 file descriptor, async task와 메모리가 증가할 수 있다. `maxConcurrentTasks`는 실제 target 실행
수만 제한하므로 UDS transport 자원까지 보호하지 않는다.

## 검토한 선택지

| 선택지 | 장점 | 단점 |
|---|---|---|
| `maxConcurrentTasks`를 연결 상한으로 재사용 | 설정이 하나임 | 긴 polling 연결과 실행 슬롯의 의미가 섞이고 Protocol capability 의미가 바뀜 |
| 초과 연결을 내부 대기열에 보관 | 연결 성공을 더 많이 유지함 | file descriptor 사용을 제한하지 못하고 MVP의 대기열 없는 원칙과 어긋남 |
| 별도 서비스 설정으로 제한하고 초과 연결을 즉시 종료 | transport 자원을 직접 제한하고 wire 계약을 바꾸지 않음 | 클라이언트는 protocol 오류가 아닌 연결 종료를 처리해야 함 |

## 결정

TaskCage MVP는 0보다 큰 `max-concurrent-connections`를 `taskcaged serve`의 필수 서비스 설정으로
사용한다. 공개 기본값은 두지 않으며 배포자가 환경에 맞는 값을 명시한다.

- 이 값은 `maxConcurrentTasks`와 독립적이다. `getCapabilities`에 새 field를 추가하지 않는다.
- 연결 슬롯 하나는 accept된 연결의 handler가 시작될 때 사용한다. 한 연결에서 여러 요청을 순서대로
  처리해도 슬롯은 하나만 사용한다.
- 실행 중인 handler와 실행이 끝났지만 아직 `JoinSet`에서 회수되지 않은 handler를 합친 수가 상한을
  넘지 않아야 한다.
- 완료된 handler를 새 연결 수락보다 먼저 회수한다. 짧은 연결이 반복돼도 완료 항목이 계속 쌓이거나
  슬롯 회수가 굶지 않게 한다.
- 상한에 도달한 뒤 accept한 연결은 frame prefix, JSON과 요청 본문을 읽지 않고 즉시 닫는다. dispatcher,
  Registry, cgroup과 target process side effect는 만들지 않는다.
- 초과 연결을 기다리게 하는 FIFO 또는 비동기 대기열은 만들지 않는다.
- 초과 연결에는 Protocol v1 응답을 쓰지 않는다. 클라이언트는 EOF 또는 연결 reset을 transport 실패로
  처리하며 필요하면 자체 재연결 정책을 적용한다.
- 기존에 슬롯을 얻은 연결은 새 연결의 거절로 영향을 받지 않는다.

## 슬롯 회수

정상 EOF, frame 오류, schema 오류, 응답 쓰기 실패, client 연결 종료와 handler panic에서 handler를
반드시 join한 뒤 슬롯을 다시 사용할 수 있다. 정상 shutdown은 listener를 닫고 모든 handler를 중단·회수한
뒤 끝난다. fail-stop은 기존 절대 deadline 안에서 연결을 drain하고, 남은 handler를 중단·회수한 뒤
비정상 종료한다. 두 종료 경로 모두 연결 handler를 백그라운드에 남기지 않는다.

한도 초과 로그는 요청 내용 없이 누적 거절 수와 설정 상한만 기록할 수 있다. 명령 인자, 환경 변수와
출력 원문은 기록하지 않는다.

## 선택 이유

- 작업 capacity와 transport capacity의 소유권과 의미를 분리한다.
- 느린 partial frame 연결도 명시적인 file descriptor·task 상한 안에 둔다.
- protocol field나 오류 코드를 추가하지 않고 기존 SDK의 transport 실패 처리로 표현한다.
- accept 자체를 멈추지 않으므로 kernel backlog에 초과 연결을 무제한으로 대기시키지 않고 빠르게 닫는다.

## 영향과 남은 위험

- 배포자는 작업 수, SDK 연결 풀과 운영 환경의 file descriptor 상한을 고려해 값을 정해야 한다.
- 연결 한도 초과와 daemon 중단은 wire 응답만으로 구분되지 않는다. 운영 관측은 요청 내용 없는 내부
  metric 또는 집계 로그로 보강할 수 있다.
- 이 결정은 UID 기반 authorization, 원격 통신, TCP와 TLS를 추가하지 않는다.
- Java SDK wire 모델은 바뀌지 않는다. Java 공개 API와 연결 풀 정책은 별도 팀 작업이다.

## 검증 방법

- 상한 N에서 N개 handler만 dispatcher에 진입하고 N+1 연결은 응답 없이 종료되는지 확인한다.
- 초과 연결이 Registry, cgroup 또는 process side effect를 만들지 않는지 확인한다.
- EOF, frame 오류, write 오류, panic, 정상 shutdown과 fail-stop에서 슬롯이 회수되는지 확인한다.
- 짧은 연결 반복과 partial prefix 연결에서 handler 수와 file descriptor가 상한에 묶이는지 확인한다.
- 실제 Linux `taskcaged serve` 프로세스와 UDS로 한도 및 정상 종료를 검증한다.

## 관련 작업

- 구현 Issue: [#72](https://github.com/taskcage/taskcage/issues/72)
- 명시적 socket 경로와 권한: [ADR 0004](0004-explicit-owner-only-uds-socket.md)
- 시작 복구 소유권: [ADR 0005](0005-own-startup-recovery-before-removing-stale-socket.md)
- protocol 계약: [MVP API 명세](../api-mvp.md)
