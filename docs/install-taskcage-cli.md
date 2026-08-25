# TaskCage CLI 설치

`taskcage`는 Capsule 작성자가 `Capsulefile`을 `.tccapsule` Pack으로 만드는 독립 CLI다. Linux daemon을
설치하거나 cgroup 권한을 부여하지 않아도 된다. 다만 `FROM oci://...` Runtime source를 사용하는 build에는
Docker CLI와 해당 registry 접근 권한이 필요하다.

CLI source는 daemon과 같은 Rust package에서 관리되므로 같은 version을 사용한다. 배포와 GitHub Release는
별도 tag인 `taskcage-v<version>`으로 생성된다.

## 설치

GitHub Release에서 현재 플랫폼에 맞는 archive와 `.sha256` 파일을 내려받아 checksum을 검증한다.

```bash
VERSION=0.7.0
TARGET=aarch64-apple-darwin # Linux x86-64: x86_64-unknown-linux-gnu
BASE_URL="https://github.com/taskcage/taskcage/releases/download/taskcage-v${VERSION}"

curl --fail --location --remote-name "${BASE_URL}/taskcage-cli-v${VERSION}-${TARGET}.tar.gz"
curl --fail --location --remote-name "${BASE_URL}/taskcage-cli-v${VERSION}-${TARGET}.tar.gz.sha256"
shasum -a 256 --check "taskcage-cli-v${VERSION}-${TARGET}.tar.gz.sha256"
tar -xzf "taskcage-cli-v${VERSION}-${TARGET}.tar.gz"
install -m 0755 "taskcage-cli-v${VERSION}-${TARGET}/bin/taskcage" /usr/local/bin/taskcage
taskcage capsule build
```

Linux에서는 `shasum -a 256` 대신 `sha256sum --check`를 사용할 수 있다. 마지막 명령은 필수 옵션이 없다는
usage error를 내지만, binary가 정상 실행되는지 빠르게 확인하는 용도다.

## Capsule Pack 만들기

`Capsulefile` 문법은 archive의 `CAPSULEFILE.md`와 [Capsulefile과 Capsule Pack](capsule-builder.md)을
참고한다. OCI Runtime source를 사용하는 최소 예시는 다음과 같다.

```text
FROM oci://registry.example.org/taskcage/ffmpeg-runtime@sha256:<64-hex-digest>
RUNTIME ID org.example.ffmpeg VERSION 7.1.1 ENTRYPOINT usr/bin/ffmpeg GLIBC 2.35 SBOM usr/share/doc/ffmpeg/sbom.spdx.json

CAPSULE ffmpeg-audio-to-wav@1.0.0
INPUT source ARTIFACT
OPTION sampleRateHz INT ALLOWED 8000,16000,22050,44100,48000
OUTPUT audio FILE result.wav MEDIA_TYPE audio/wav MAX_BYTES 1073741824
COMMAND -i ${source} -ar ${sampleRateHz} ${audio}
LIMIT CPU 1 MEMORY 512MiB PIDS 32 TIMEOUT 2m
```

```bash
taskcage capsule build Capsulefile \
  --platform linux/arm64 \
  --output ffmpeg-audio-to-wav-1.0.0-linux-arm64.tccapsule
```

Pack을 daemon operator에게 전달하면 Linux daemon host에서 service account로 설치한다. 이 단계에도 같은
version의 `taskcage` CLI archive가 필요하다.

```bash
sudo -u taskcage taskcage capsule install ffmpeg-audio-to-wav-1.0.0-linux-arm64.tccapsule
```

`capsule install`은 Linux daemon host에서만 지원한다. macOS와 Windows의 CLI는 Pack을 만드는 authoring
도구로 사용한다.
