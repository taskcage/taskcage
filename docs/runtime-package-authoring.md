# Runtime Package 작성 계약 v1

## 목적

Runtime Package는 Capsule이 실행할 Linux binary와 필요한 파일을 검증 가능한 단위로 전달한다.
Capsulefile은 **어떻게 호출할지**를 정의하고, Runtime Package는 **무엇을 실행할지**를 제공한다.

```text
Runtime source directory
        │ package authoring
        ▼
Runtime Package (target-specific)
        │ taskcage capsule build
        ▼
Capsule Pack (.tccapsule)
```

`taskcage runtime build`는 준비한 target rootfs에서 Runtime Package directory와 manifest를 만든다. 작성자는
실행 파일·target·entrypoint·glibc 최소 버전·license/SBOM 위치를 선언하고, CLI가 file digest·size·안전한 mode를
생성한다. `taskcage capsule build`는 완성된 Package를 다시 해석하거나 host에서 실행하지 않고,
manifest·file digest·platform 조건을 확인한 뒤 Pack에 포함한다.

## 지원 target

첫 target은 다음 둘로 제한한다.

| TaskCage platform | Runtime Package architecture | 대상 |
| --- | --- | --- |
| `linux/amd64` | `x86_64` | Intel/AMD 64-bit Linux |
| `linux/arm64` | `aarch64` | ARM 64-bit Linux |

같은 Capsule identity라도 target별 Package와 `.tccapsule`을 각각 만든다. 작성자의 macOS, Windows 또는
Linux host가 target과 같을 필요는 없지만, **실제 target용 binary와 dependency file set**을 제공해야 한다.
TaskCage는 binary cross compilation이나 OCI image 추출을 대신하지 않는다.

## source layout

```text
ffmpeg-runtime-linux-arm64/
├── runtime-package.json
└── rootfs/
    ├── bin/ffmpeg
    ├── lib/...
    └── share/
        ├── licenses/...
        └── sbom.spdx.json
```

`rootfs`에는 regular file과 directory만 포함할 수 있다. symlink, device, FIFO와 host absolute path는
허용하지 않는다. `runtime-package.json`은 모든 file의 SHA-256·size·read-only/executable mode, entrypoint,
library path, license, SBOM과 target platform을 선언한다.

daemon은 import 때 이 선언과 실제 file set을 다시 확인하고, digest로 Runtime Package cache를 공유한다.
같은 digest를 참조하는 여러 Capsule은 Runtime 파일을 중복 저장하지 않는다.

## 생성 명령

```bash
taskcage runtime build \
  --source-rootfs ./ffmpeg-rootfs \
  --output ./ffmpeg-runtime-linux-arm64 \
  --id org.example.ffmpeg \
  --version 7.1.0 \
  --platform linux/arm64 \
  --glibc-minimum 2.35 \
  --entrypoint bin/ffmpeg \
  --library-path lib \
  --sbom share/sbom.spdx.json
```

`--source-rootfs`에는 target용 binary, shared library, license와 SPDX JSON이 준비돼 있어야 한다.
`--license Apache-2.0:share/licenses/ffmpeg.txt`는 필요할 때 반복 지정한다. 생성 결과는 다음 단계에서
`taskcage capsule build Capsulefile --runtime-package ./ffmpeg-runtime-linux-arm64 ...`에 전달한다.

## 작성자 책임과 TaskCage 책임

| 구분 | 책임 |
| --- | --- |
| Package 작성자 | target binary·library·license·SBOM의 출처와 호환성을 검증한다. |
| Capsulefile 작성자 | 입력/출력, 제한, 안전한 argv template을 정의한다. |
| `taskcage capsule build` | Package와 Capsule 계약의 digest·platform 일치를 검증하고 Pack을 만든다. |
| daemon | Pack 안전성, checksum, platform과 cgroup 실행·정리를 검증한다. |

## 이후 범위

OCI registry 또는 공식 Runtime registry에서 target별 source를 조회하고 Package를 만드는 기능은 이후
별도 작업이다. v1은 임의 URL 다운로드나 host의 `/usr/bin` 참조를 자동으로 허용하지 않는다. 이 경계를
유지해야 Pack이 재현 가능한 실행 계약으로 남는다.
