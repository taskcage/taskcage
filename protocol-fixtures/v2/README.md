# Product Alpha Artifact fixture

이 디렉터리는 아직 구현되지 않은 Protocol v2 전체 wire operation을 선언하지 않는다. #149에서 합의한 Local
Artifact descriptor와 거절 조건만 고정한다. `submitProfile`과 `profileResult`의 전체 fixture는 #150에서
이 descriptor를 사용해 추가한다.

- `artifact-input-valid.json`: canonical local input descriptor
- `artifact-input-invalid-path.json`: traversal path는 target 시작 전 거절한다
- `artifact-input-digest-mismatch.json`: changed source는 target 시작 전 거절한다
- `artifact-output-undeclared.json`: 실행 뒤 undeclared output은 publish하지 않는다
