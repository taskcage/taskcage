# Local Profile Core v2 fixture

이 디렉터리는 v0.2 Local Profile Core의 승인된 wire fixture를 담는다. #149의 Local Artifact descriptor와
#156의 Profile request/result shape를 함께 고정하지만, 현재 daemon이나 Java SDK가 Protocol v2를 구현했다는
뜻은 아니다. #150과 #153은 이 파일들을 각각 Rust/Java conformance 및 Linux real-daemon E2E에서 검증한다.

- `artifact-input-valid.json`: canonical local input descriptor
- `artifact-input-invalid-path.json`: traversal path는 target 시작 전 거절한다
- `artifact-input-digest-mismatch.json`: changed source는 target 시작 전 거절한다
- `artifact-output-undeclared.json`: 실행 뒤 undeclared output은 publish하지 않는다
- `submit-profile-valid.json`: generic ProfileRequest와 모든 supported input kind
- `submit-profile-invalid-input.json`: unknown input slot은 target 시작 전 거절한다
- `profile-accepted.json`: cgroup 안에서 target이 시작된 뒤의 v2 acceptance
- `get-profile-result.json`: Profile Task 결과 조회 request
- `profile-result-running.json`: running Profile Task snapshot
- `profile-result-success.json`: cleanup-confirmed output Artifact success
- `profile-result-output-contract-failed.json`: output violation은 Artifact를 publish하지 않는다
- `error-profile-not-found.json`: installed Profile이 없는 pre-execution rejection
- `error-artifact-digest-mismatch.json`: preflight snapshot mismatch rejection
