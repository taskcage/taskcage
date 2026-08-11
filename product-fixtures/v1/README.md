# TaskCage v0.2 Product Alpha fixtures

이 디렉터리는 Local Execution Profile, Runtime Package, TaskCage Bundle 초안의 규범적 JSON 계약 fixture를
담는다. Rust daemon, Java SDK와 import tooling은 같은 구조와 validation 의미를 구현해야 한다.

- `ffmpeg-transcode-profile.json`: v0.2에서 하나만 제공하는 표준 Execution Profile
- `ffmpeg-runtime-package.json`: 별도로 import하고 digest 기준으로 공유하는 Runtime Package manifest
- `ffmpeg-transcode-bundle.json`: Profile을 inline하고 Package는 identity와 digest로만 참조하는 Bundle

호출자는 output 경로를 요청에 전달하지 않는다. FFmpeg Profile이 output slot과 고정 파일명
`result.mp4`를 선언하고 daemon이 Task별 공개 경로를 결정한다.

Bundle에는 executable, library, codec, font 또는 다른 Package bytes가 들어가지 않는다. 여러 Bundle은
같은 Runtime Package digest를 참조해 cache entry 하나를 공유할 수 있다.

Profile, Runtime Package와 Bundle digest는 각각 자기 manifest 전체를 RFC 8785로 canonicalize한 JSON
bytes의 SHA-256이다. 자기 digest field는 schema에 두지 않아 순환 계산을 만들지 않으며, 검증된 import
결과, digest-addressed cache path와 요청 또는 Bundle의 참조가 digest를 보유한다.

이 파일은 구현 목표와 호환성 계약이지 현재 release의 availability 주장이 아니다. Bundle과 Protocol
fixture가 참조하는 Profile, Runtime Package와 Bundle digest는 이 세 manifest의 canonical SHA-256과
일치한다. Runtime Package의 개별 payload file digest는 실제 Package bytes가 없는 형식 fixture를 위한
결정적인 값이다.
