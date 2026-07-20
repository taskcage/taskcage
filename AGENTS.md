# AGENTS.md

TaskCage는 Linux에서 무거운 외부 프로그램을 작업 단위로 실행·제한·관찰·정리하는 Rust 기반 관리 프로그램과 Java SDK다.

## 먼저 읽을 문서

- [README.md](README.md): 프로젝트 목표, 지원 범위, 핵심 아키텍처와 API 방향
- [CONTRIBUTING.md](CONTRIBUTING.md): 브랜치, 커밋, PR, 리뷰 규칙 (파일이 추가된 뒤 적용)
- `docs/decisions/`: 큰 기술적 선택의 근거와 결과

## 예상 구조

- `daemon/`: cgroup v2와 Linux 프로세스를 관리하는 Rust 데몬
- `java-sdk/`: Java 애플리케이션에서 데몬을 호출하는 SDK
- `integration-tests/`: 실제 Linux cgroup v2 환경을 검증하는 통합 테스트
- `test-fixtures/`: CPU·메모리·프로세스 제한과 정리를 검증할 대상 프로그램
- `docs/`: 설계 문서와 의사결정 기록

## 변경 원칙

- 외부 명령은 shell 문자열이 아니라 실행 파일과 인자 배열로 전달한다.
- cgroup v2 제한이 적용되지 않은 상태로 외부 명령을 실행하지 않는다.
- 시간 초과·취소·오류 시 개별 PID만 종료하지 말고 작업 cgroup 전체를 정리한다.
- 종료 원인은 단일 exit code로 추측하지 않고 cgroup 이벤트와 프로세스 상태를 함께 확인한다.
- Rust 데몬과 Java SDK 사이의 프로토콜 변경은 양쪽 구현과 호환성 테스트를 함께 갱신한다.
- 구조, 공개 API, 자원 제한 정책에 영향을 주는 변경은 `docs/decisions/` 기록 필요성을 확인한다.

## 검증

Rust 코드·설정 변경 후에는 가능한 범위에서 다음을 실행한다.

```bash
cargo fmt --check
cargo clippy -- -D warnings
cargo test
```

cgroup·프로세스 정리 동작을 바꾼 경우에는 Linux 통합 테스트로 CPU 과다 사용, 메모리 부족, 프로세스 수 제한, 시간 초과와 자식 프로세스 정리 시나리오를 검증한다.
