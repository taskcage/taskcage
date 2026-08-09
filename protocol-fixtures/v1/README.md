# TaskCage protocol fixtures v1

이 디렉터리의 JSON 파일은 API 문서의 예시이면서 Java SDK와 Rust 데몬이 공유하는 테스트 입력값이다.

- Java SDK는 요청/응답 역직렬화와 오류 매핑 테스트에 사용한다.
- Rust 데몬은 역직렬화·직렬화와 상태 전이 테스트에 사용한다.
- fixture를 바꾸면 `docs/api-mvp.md`와 양쪽 구현 테스트를 함께 갱신한다.

현재 파일은 MVP 프로토콜의 최소 정상 흐름, 실행 시작 실패, timeout 결과, 출력 tail 잘림, 실행 슬롯 부족 오류를 다룬다. `task-result-execution-failed.json`은 `submitTask`의 `execve` 시작 실패가 새 응답 타입 없이 기존 `task`/`FINISHED`와 `EXECUTION_FAILED`로 반환되는 계약을 고정한다. Linux cgroup 동작 자체를 검증하는 실행 fixture 프로그램은 별도 `test-fixtures/`에 둔다.

cgroup 제한값을 쓴 뒤 read-back 값이 요청과 다르면 기존 `error` 응답의 `INTERNAL_ERROR`와
`retryable: false`를 사용한다. 이 실패에서는 target, `taskAccepted`와 공개 `taskId`가 만들어지지
않으며, 정리를 증명할 수 없으면 기존 fail-stop 계약으로 전환한다. `LIMIT_EXCEEDS_POLICY`는 cgroup을
만들기 전의 명시적인 배포 정책 검증 실패에만 사용한다.

이 결정은 기존 wire 형식과 오류 코드 집합을 바꾸지 않으므로 새 JSON fixture를 추가하지 않는다.
현재 JSON corpus와 Java SDK 입력은 그대로다. 향후 read-back 전용 JSON fixture를 추가하려면 Rust와
Java의 fixture 집합·오류 매핑 시험을 같은 protocol 변경에서 함께 갱신한다.
