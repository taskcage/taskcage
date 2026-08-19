# Capsule execution contract v1 fixtures

이 fixture corpus는 EmbeddedRunner와 ExternalRunner가 공유해야 하는 Capsule 실행 의미를 고정한다.
Local Protocol v2의 envelope fixture와 달리 transport framing을 정의하지 않는다.

- `request-valid.json`: Capsule identity, Profile identity, typed input과 override
- `result-success.json`: output publish와 cleanup이 확인된 성공
- `result-failed.json`: exit code 0이어도 output contract 위반이면 실패
- `result-timeout.json`: timeout 뒤 whole-task cleanup이 확인된 실패
- `result-cancelled.json`: cancel 뒤 whole-task cleanup이 확인된 실패

구현체는 fixture의 필드명·identity·outcome·failure 의미를 임의로 바꾸지 않아야 한다. 추가적인
transport envelope은 Local/Remote protocol fixture에서 별도로 검증한다.
