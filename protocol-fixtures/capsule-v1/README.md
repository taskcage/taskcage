# Capsule execution contract v1 fixtures

이 fixture corpus는 EmbeddedRunner와 ExternalRunner가 공유해야 하는 Capsule 실행 의미를 고정한다.
Local Protocol v2의 envelope fixture와 달리 transport framing을 정의하지 않는다.

- `request-valid.json`: Capsule identity와 일치하는 Profile identity, typed input과 override
- `error-capsule-profile-mismatch.json`: identity 불일치의 pre-execution rejection과 side-effect 금지
- `result-success.json`: output publish와 cleanup이 확인된 성공
- `result-failed.json`: non-zero process exit 뒤 Artifact를 공개하지 않는 실패
- `result-output-contract-failed.json`: exit code 0이어도 output contract 위반이면 실패
- `result-timeout.json`: timeout 뒤 whole-task cleanup이 확인된 실패
- `result-cancelled.json`: cancel 뒤 whole-task cleanup이 확인된 실패

모든 terminal fixture는 `timing`, `usage`, bounded stdout/stderr `output`, `artifacts`와 `failure`를 가진다.
성공 fixture만 선언된 output Artifact 하나를 공개하며, 실패 fixture의 `artifacts`는 빈 object다.

구현체는 fixture의 field name, Capsule/Profile identity, outcome, Artifact visibility와 failure 의미를
임의로 바꾸지 않아야 한다. Local/Remote transport envelope과 실제 Artifact descriptor는 각 protocol
fixture에서 별도로 검증한다.
