# Local Runtime Package cache

## 상태와 범위

이 문서는 TaskCage v0.2 Local Product Alpha의 Runtime Package import와 digest cache 계약이다.
Runtime Package는 신뢰된 Linux 실행 파일과 필요한 library·license·SBOM을 하나의 검증 가능한 file set으로
고정한다. 이 기능은 Hub, URL download, 자동 update, eviction, Bundle import 또는 container image를
제공하지 않는다.

다음 Bundle-first 단계에서는 Bundle이 이 문서의 immutable Package digest를 참조한다. Bundle archive의
import와 signature 검증은 별도 계약이며, 이 cache 계약을 우회하거나 Package를 mutable path로 실행하게
해서는 안 된다. 계획상 형식은 [Bundle 형식 초안](bundle-format.md)에 있다.

지원 platform은 현재 `linux/x86_64/gnu`, `linux/aarch64/gnu`와 glibc다. Package architecture는 daemon이
실행 중인 host architecture와 정확히 일치해야 하며, Package가 요구하는 최소 glibc version이 host보다 높으면
import와 resolve가 모두 실패한다.

## 관리자 import

배포 관리자는 daemon과 같은 service UID로 local source directory를 명시적으로 import한다. cache root와
그 하위 entry는 import process의 effective UID가 소유해야 하므로 root로 import한 cache를 `taskcage`
daemon이 소비하는 경로는 허용하지 않는다.

```bash
sudo -u taskcage taskcaged import-package \
  --source /srv/taskcage-import/ffmpeg-7.1.1 \
  --cache-root /var/lib/taskcage
```

성공 결과는 계산된 canonical digest와 `IMPORTED` 또는 `ALREADY_PRESENT` outcome을 JSON으로 반환한다.
daemon Task 요청 중 network나 host package manager를 통해 Package를 자동 설치하지 않는다.

source layout은 정확히 다음 두 entry만 가져야 한다.

```text
runtime-package.json
rootfs/
  bin/tool
  lib/...
  share/...
```

`rootfs`에는 manifest가 선언한 regular file과 그 parent directory만 존재해야 한다. symlink, hardlink,
FIFO, socket, device, mount crossing, 빠진 file과 선언하지 않은 file은 거부한다.

## Manifest

`runtime-package.json`은 최대 1 MiB이며 unknown field를 거부한다. 전체 shape는 다음과 같다.

```json
{
  "schemaVersion": "taskcage.runtime-package/v0alpha1",
  "id": "org.taskcage.ffmpeg",
  "version": "7.1.1-taskcage.1",
  "platform": {
    "os": "linux",
    "architecture": "x86_64",
    "abi": "gnu",
    "libc": {
      "family": "glibc",
      "minimumVersion": "2.39"
    }
  },
  "entrypoint": "bin/ffmpeg",
  "libraryPaths": ["lib"],
  "files": [
    {
      "path": "bin/ffmpeg",
      "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "sizeBytes": 82739200,
      "mode": "0555"
    },
    {
      "path": "share/licenses/ffmpeg/COPYING.GPLv2",
      "digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      "sizeBytes": 18092,
      "mode": "0444"
    },
    {
      "path": "share/sbom.spdx.json",
      "digest": "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
      "sizeBytes": 16384,
      "mode": "0444"
    }
  ],
  "licenses": [
    {
      "spdxId": "GPL-2.0-or-later",
      "path": "share/licenses/ffmpeg/COPYING.GPLv2"
    }
  ],
  "sbom": {
    "format": "SPDX-JSON-2.3",
    "path": "share/sbom.spdx.json"
  }
}
```

- `id`는 점으로 구분한 lowercase identity, `version`은 canonical SemVer다.
- `entrypoint`, `libraryPaths`, `files.path`, license와 SBOM path는 `rootfs` 기준 canonical 상대 경로다.
- `files`는 path UTF-8 byte 순으로 정렬하고 중복할 수 없다. 최대 4,096개다.
- file mode는 read-only `0444` 또는 executable read-only `0555`다. `entrypoint`는 `0555`여야 한다.
- 각 file의 실제 type, link count, size, mode와 SHA-256 digest가 선언과 같아야 한다.
- `sizeBytes`는 언어 사이에서 정확히 공유할 수 있는 JSON 정수 범위인 `0..=9007199254740991`이다.
- `libraryPaths`는 최대 64개이며 각 path 아래에 선언된 file이 있어야 한다.
- `licenses`는 최대 256개이고 `files`의 path를 참조한다. SBOM은 `SPDX-JSON-2.3` file을 참조한다.

Manifest 자체에는 Package digest field를 넣지 않는다. importer는 검증된 manifest를 RFC 8785로
canonicalize한 bytes에서 Package digest를 계산한다. 따라서 manifest와 그 안의 모든 file digest가 하나의
Package identity로 고정된다.

## Cache와 실행 경계

논리 cache layout은 다음과 같다.

```text
<cache-root>/packages/sha256/<64-lowercase-hex>/
  runtime-package.json
  rootfs/...
```

Importer는 daemon 소유 staging directory에 file을 copy하면서 digest를 검증하고 file과 directory를
`fsync`한다. 완전한 entry만 `renameat2(RENAME_NOREPLACE)`로 활성화하며 이 기능을 증명할 수 없는
filesystem에서는 실패한다. 그러므로 concurrent reader는 partial Package를 볼 수 없다.

같은 digest가 이미 있으면 기존 manifest와 모든 선언 file을 다시 검증한 뒤 `ALREADY_PRESENT`를 반환한다.
기존 entry가 손상됐으면 덮어쓰지 않고 실패한다. 여러 Profile은 같은 digest entry를 공유한다.

Task 실행 경로는 Package를 digest로 열고 manifest, platform과 전체 content를 다시 검증한다. 검증된
rootfs와 entrypoint file descriptor를 실행 준비가 끝날 때까지 보유하므로 cache path가 바뀌어도 검증하지
않은 inode로 전환되지 않는다. Protocol v1 Raw Command의 absolute executable 동작은 변경하지 않는다.

## FFmpeg Profile 정적 등록

`ffmpeg-audio-to-wav@1.0.0`은 generic registry 없이 하나의 cache root와 digest를 정적으로 등록한다.
AMD64와 ARM64 FFmpeg Package는 서로 다른 binary content와 digest를 가지므로, 각 host에는 자신의
architecture에 맞는 digest를 설정한다.
Artifact 설정과 아래 두 옵션을 모두 지정해야 한다.

```text
--runtime-package-cache-root /var/lib/taskcage
--ffmpeg-audio-to-wav-package-digest sha256:<64-lowercase-hex>
```

daemon은 시작할 때 등록 digest를 resolve하고 package `id`가 `org.taskcage.ffmpeg`, `entrypoint`가
`bin/ffmpeg`인지 확인한다. missing, incompatible, corrupted package와 계약 불일치는 daemon 시작 실패다.
각 새 Task도 같은 digest의 manifest, platform, 전체 content를 다시 검증하고 고정 entrypoint descriptor를
`execveat(AT_EMPTY_PATH)`로 실행한다. shell과 PATH lookup은 사용하지 않는다.
