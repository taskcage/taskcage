# TaskCage Remote Protocol v1 fixture

이 디렉터리의 JSON은 [Remote Protocol v1](../../docs/remote-protocol-v1.md)의 실행 가능한 예시이자 Rust
daemon과 언어 SDK가 공유하는 wire compatibility 계약이다. 이 fixture는 TLS handshake 자체나 실제 secret을
담지 않는다. TLS와 secret verifier는 각 구현체의 통합 테스트에서 검증한다.

- `authenticate-request.json`, `authenticated.json`: TLS 뒤 첫 frame의 service-account 인증
- `get-capabilities.json`, `capabilities.json`: 인증 뒤 Remote capability 확인
- `begin-artifact-upload.json`, `artifact-upload-started.json`, `upload-artifact-chunk.json`, `complete-artifact-upload.json`, `abort-artifact-upload.json`, `artifact-uploaded.json`: managed input upload lifecycle
- `read-artifact-chunk.json`, `artifact-chunk.json`: managed output download lifecycle
- `submit-profile-valid.json`, `profile-accepted.json`: managed input Artifact를 이용한 Profile 수락
- `get-profile-result.json`, `profile-result-running.json`, `profile-result-success.json`, `profile-result-failed.json`: Profile 결과 상태
- `cancel-task.json`, `task-cancelled.json`: 인증된 principal의 cleanup-confirmed 취소
- `error-*.json`: 모든 안정 Remote 오류 코드

fixture의 `secret`과 digest는 테스트 전용 값이다. 실제 secret, 인증서, private key 또는 production Artifact를
추가하면 안 된다. frame, field, enum, error 의미를 바꾸면 Remote Protocol 문서, daemon conformance test, SDK
encoder/decoder test와 real TLS E2E를 같은 변경에서 갱신한다.
