# TaskCage Protocol v1 fixture

이 디렉터리의 JSON은 [Protocol v1 API 명세](../../docs/api-mvp.md)의 실행 가능한 예시이자 Rust 데몬과 Java SDK가 공유하는 호환성 계약이다.

현재 fixture는 다음을 고정한다.

- 유효한 `submitTask` 요청과 `taskAccepted`
- `RUNNING` snapshot
- exec 시작 실패와 timeout 결과
- stdout/stderr tail truncation 결과
- 실행 capacity 부족 오류
- 배포 자원 정책 초과 오류

fixture를 바꾸면 API 명세, Rust 직렬화·상태 전이 테스트, Java 역직렬화·오류 매핑 테스트를 같은 변경에서 갱신한다. cgroup 제한과 프로세스 정리의 실제 커널 동작은 `test-fixtures/`와 `integration-tests/`에서 검증한다.

v0.2 Product Alpha의 Profile 작업은 이 v1 계약을 확장하거나 재해석하지 않는다. 별도 Protocol v2 계약과
fixture는 [`../v2/`](../v2/README.md)에 둔다.
