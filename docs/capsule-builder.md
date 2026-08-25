# Capsulefile과 Capsule Pack v1

## 목적

Capsule은 외부 CLI를 실행하는 방법을 공유 가능한 계약으로 만든다. 작성자는 사람이 읽는
`Capsulefile`을 관리하고, `taskcage capsule build`가 daemon이 설치할 수 있는 불변 Capsule Pack을 만든다.

```text
Capsulefile + Runtime source
        │ taskcage capsule build
        ▼
<name>-<version>-linux-<architecture>.tccapsule
        │ taskcage capsule install
        ▼
daemon-owned Capsule catalog + Runtime Package cache
```

`taskcage`는 작성·검사·설치용 짧게 실행되는 CLI다. `taskcaged serve`는 요청을 받아 Capsule을 실행하는
장기 실행 daemon이다. 두 프로그램은 같은 Rust codebase와 Capsule 계약을 공유하지만 역할은 다르다.

## 기본 모드

첫 공개 흐름은 조직 내부나 개발자가 직접 전달한 Pack을 빠르게 사용하게 하는 **기본 모드**다.

- Pack install은 서명 없이 허용한다.
- importer는 archive 안전성, manifest/schema, checksum, Runtime Package digest와 platform 호환성은
  항상 검증한다.
- Pack install은 daemon host의 로컬 operator만 수행한다. SDK의 TLS/UDS 요청으로 Pack을 upload하거나
  install하지 않는다.
- 인증된 remote principal은 daemon에 이미 설치된 Capsule을 실행할 수 있다. 기본 모드는
  Capsule별 principal allowlist를 강제하지 않는다.

서명 검증과 trust store, Capsule별 실행 allowlist는 조직 간 공유·운영 요구가 확인된 뒤의 강화 모드다.
기본 모드의 무서명 Pack은 신뢰할 수 없는 code를 안전하게 만드는 기능이 아니다.

## Capsulefile 최소 문법

Capsulefile은 shell script가 아니다. `COMMAND`는 개별 argv token의 목록이며 shell, PATH lookup,
environment assignment, redirection, pipe, glob, 조건문과 임의 host path를 허용하지 않는다.

```text
FROM runtime://example.org/ffmpeg-runtime:7.1.0
CAPSULE ffmpeg-audio-to-wav@1.0.0

INPUT source ARTIFACT
OPTION sampleRateHz INT ALLOWED 8000,16000,22050,44100,48000
OPTION channels INT ALLOWED 1,2
OUTPUT audio FILE result.wav MEDIA_TYPE audio/wav MAX_BYTES 1073741824

COMMAND -hide_banner -loglevel error -nostdin \
  -i ${source} \
  -map 0:a:0 -vn \
  -c:a pcm_s16le \
  -ar ${sampleRateHz} \
  -ac ${channels} \
  ${audio}

LIMIT CPU 1 MEMORY 512MiB PIDS 32 TIMEOUT 2m
ALLOW OVERRIDE MEMORY,TIMEOUT
```

OCI Runtime을 Capsulefile에서 직접 가져올 때는 `FROM`과 함께 Runtime Package metadata를 선언한다.

```text
FROM oci://registry.example/ffmpeg-runtime@sha256:<64-hex-digest>
RUNTIME ID org.example.ffmpeg VERSION 7.1.0 ENTRYPOINT bin/ffmpeg GLIBC 2.35 \
  LIBRARY_PATH lib SBOM share/sbom.spdx.json
```

| Directive | 책임 |
| --- | --- |
| `FROM` | 설치된 Runtime Package identity 또는 digest 고정 OCI Runtime source를 가리킨다. |
| `RUNTIME` | OCI source를 Package로 만들 때 필요한 immutable Runtime metadata를 선언한다. |
| `CAPSULE` | immutable `name@version` identity를 선언한다. |
| `INPUT`, `OPTION` | Artifact 또는 typed scalar 입력과 허용값을 선언한다. |
| `OUTPUT` | daemon이 publish할 파일 결과 하나를 선언한다. |
| `COMMAND` | 검증된 Runtime entrypoint에 전달할 안전한 argv template을 선언한다. |
| `LIMIT`, `ALLOW OVERRIDE` | Capsule 기본 resource budget과 caller가 더 제한적으로 바꿀 수 있는 범위를 선언한다. |

v1은 한 Runtime Package, 한 Profile, 한 input Artifact, caller가 명시한 scalar option, 한 output Artifact를 지원한다.
현재 Profile contract는 모든 option을 필수로 검증한다. Capsule-level option default는 이후 Profile schema 확장으로
추가하며, Builder v1은 default를 조용히 무시하지 않는다.
결과 파일의 도메인 의미(예: 오디오 내용이 업무적으로 올바른가)는 Capsule의 책임이 아니다. 성공은
exit code 0, 선언 output의 존재·상한, atomic artifact publish, whole-process cleanup 확인으로 고정한다.

## Runtime source와 platform

Runtime Package는 실제 실행 binary와 필요한 shared library·설정 파일을 가진 검증 가능한 Linux file set이다.
`FROM`은 Capsule이 요구하는 Runtime identity를 선언하고, builder에는 선택한 target platform의 검증 가능한
Runtime Package directory를 명시적으로 전달한다. builder는 이를 Pack에 포함하며 daemon은 install 또는 실행 중에
network에서 Runtime을 내려받지 않는다.

`FROM oci://...`의 경우 `taskcage capsule build`가 선택한 platform을 Docker OCI client로 내려받아 Runtime
Package를 먼저 만든다. 이 경로에서는 `--runtime-package`를 지정하지 않는다.

첫 지원 대상은 다음 두 개다.

```text
linux/amd64  (x86_64, glibc)
linux/arm64  (aarch64, glibc)
```

Capsule identity 하나는 platform마다 독립된 Pack artifact를 가질 수 있다. 예를 들어
`ffmpeg-audio-to-wav@1.0.0`은 `linux/amd64`와 `linux/arm64` Pack으로 배포된다. builder는 작성자의
macOS·Windows·Linux host와 무관하게 target platform을 명시적으로 선택할 수 있어야 한다. Runtime source가
이미 TaskCage Runtime Package 형태라면 Docker는 build의 필수 의존성이 아니다. 일반 OCI/Docker image를
Runtime Package로 변환하는 기능은 이후 범위다.

Runtime Package directory의 target·file layout·작성자 책임은 [Runtime Package 작성 계약](runtime-package-authoring.md)에서
정의한다.

## Pack 형식과 install

사용자가 주고받는 확장자는 `.tccapsule`이다.

```text
ffmpeg-audio-to-wav-1.0.0-linux-arm64.tccapsule
```

Pack은 gzip POSIX tar이며 Capsule archive와 Runtime Package를 함께 가진 self-contained 배포물이다.
daemon은 Runtime Package를 digest cache에 한 번만 저장하므로 여러 Capsule이 같은 Runtime을 재사용한다.

```bash
taskcage capsule build Capsulefile \
  --runtime-package ./ffmpeg-runtime-linux-arm64 \
  --platform linux/arm64 \
  --output ./ffmpeg-audio-to-wav-1.0.0-linux-arm64.tccapsule
taskcage capsule install ./ffmpeg-audio-to-wav-1.0.0-linux-arm64.tccapsule
taskcaged serve ...
```

현재 reader의 legacy `bundle.json`/`profile.json` 및 `.tccapsule.tar.gz` layout은 기존 Pack을 읽기 위한
호환 경로로 유지한다. 새 CLI와 문서는 Capsulefile·`.tccapsule`을 기본 명칭으로 사용한다. legacy reader는
다음 breaking release에서 제거 여부를 결정한다.

## 의도적으로 제외하는 것

- Hub 검색·자동 install·자동 update
- generic Docker image의 임의 추출과 변환
- Pack 안의 secret, trust anchor 또는 remote credential
- arbitrary executable, shell command, raw argv API
- 다중 output orchestration, 범용 stdout parser, domain-specific output validator

이 범위를 유지해야 Pack이 "프로그램 전체 환경"이 아니라, 신뢰된 외부 CLI의 재현 가능한 호출 계약으로
남는다.
