# TaskCage Protocol v2 fixtures

이 디렉터리의 JSON은 v0.2 Local Product Alpha에서 추가하는 Profile 실행 wire 계약의 규범적
호환성 fixture다. Protocol v2는 Local UDS의 `submitProfile`과 `getProfileResult`에만 적용한다.
`submitTask`, `getTask`, `cancelTask`를 포함한 Raw Command 계약은 Protocol v1을 그대로 사용한다.

fixture가 고정하는 계약은 다음과 같다.

- 유효한 `submitProfile`, 시작 확인인 `profileAccepted`, `getProfileResult` 요청과 `RUNNING` snapshot
- 정리와 Artifact 공개가 끝난 성공 `profileResult`/`FINISHED`
- `profile`은 `id`, `version`, `digest`로 고정하고 `bundleDigest`는 별도 필수 필드로 전달한다.
- `inputs`는 string, integer, boolean scalar만 허용한다.
- 요청의 `artifacts`에는 root-relative 경로를 가진 `LOCAL_INPUT`만 전달한다.
- output slot과 공개 방식은 Execution Profile이 선언하며 호출자는 output 경로를 지정하지 않는다.
- 성공 결과는 실제 Runtime Package identity와 공개된 `LOCAL_FILE`의 경로, 크기, digest, media type을 반환한다.
- Profile 입력, Artifact, Runtime Package 무결성 오류는 Task 실행 전의 구조화된 오류로 반환한다.

이 fixture는 Rust daemon과 언어별 SDK가 함께 구현해야 할 계약을 정의하지만, 현재 release에서 해당 기능을
이미 사용할 수 있다는 availability 주장은 아니다. Profile, Runtime Package와 Bundle digest는
`product-fixtures/v1/` manifest의 canonical SHA-256과 일치한다. input/output Artifact와 resolved plan
digest는 실제 payload가 없는 wire 예시를 위한 결정적인 값이다.
