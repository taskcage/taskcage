# TaskCage protocol fixtures v1

이 디렉터리의 JSON 파일은 API 문서의 예시이면서 Java SDK와 Rust 데몬이 공유하는 테스트 입력값이다.

- Java SDK는 요청/응답 역직렬화와 오류 매핑 테스트에 사용한다.
- Rust 데몬은 역직렬화·직렬화와 상태 전이 테스트에 사용한다.
- fixture를 바꾸면 `docs/api-mvp.md`와 양쪽 구현 테스트를 함께 갱신한다.

현재 파일은 MVP 프로토콜의 최소 정상 흐름, timeout 결과, 출력 tail 잘림, 실행 슬롯 부족 오류를 다룬다. Linux cgroup 동작 자체를 검증하는 실행 fixture 프로그램은 별도 `test-fixtures/`에 둔다.
