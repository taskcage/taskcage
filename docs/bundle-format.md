# TaskCage Capsule archive format v0alpha1

> 제품 용어는 **Capsule**이다. 이 문서와 현재 구현의 `bundle.json`, `profile.json`,
> `taskcage.bundle/v0alpha1` schema 값은 기존 archive·wire 호환성을 위해 Bundle 명칭을 유지한다.
> 새 사용자 문서에서는 이 archive를 Capsule로 부른다.

> **릴리스 상태:** 이 계약과 `taskcaged bundle import`, immutable local catalog, `--bundle-cache-root` 기반
> Profile 실행은 daemon `0.5.0`에서 공개된다. 공개 daemon `0.4.0`에는 `bundle` 명령과
> `--bundle-cache-root`가 없으며, `0.4.0`의 FFmpeg Profile은
> [정적 Runtime Package 등록](runtime-package-cache.md#daemon-040의-ffmpeg-profile-정적-등록)을 사용한다.
>
> **범위:** 이 문서는 `main`의 Local Bundle 실행 계약을 고정한다. Profile Task wire API는
> [Local Profile Core API v2](api-profile-v2.md)를 계속 사용한다. Hub, 자동 다운로드, Bundle payload 안의
> Runtime Package와 Remote Bundle 설치는 포함하지 않는다.

## 목적

TaskCage Capsule archive는 특정 외부 프로그램 작업을 실행하기 위한 불변 계약이다. Capsule은 실행 파일 경로나
caller-provided argv를 노출하지 않고, 어떤 Profile을 어떤 Runtime Package와 정책으로 실행할지 정의한다.

```text
Capsule = what and how to execute
Runtime Package = executable files and dependencies
Task = one cgroup-managed execution of a Capsule Profile
```

Bundle은 Docker Image가 아니다. root filesystem, PID 1, namespace, network configuration, container user와
Task별 input/output data를 포함하지 않는다.

## 배포 archive

초기 Bundle 배포물의 이름은 다음 형식이다.

```text
<bundle-name>-<bundle-version>.tcbundle.tar.gz
```

archive는 gzip으로 압축된 POSIX tar이며, 정확히 다음 네 regular file만 root에 가진다.

```text
bundle.json
profile.json
checksums.txt
signature.sig
```

각 archive는 압축 해제 전 1 MiB, file 하나는 256 KiB, 전체 file 수는 4개를 넘을 수 없다. reader는 absolute
path, 빈 path component, `.` 또는 `..`, backslash, non-UTF-8 name, duplicate path, symlink, hardlink,
device, FIFO, socket, sparse file과 예상하지 않은 tar entry를 거부한다. daemon은 검증된 staging directory에서만
archive를 처리하고, 검증이 끝난 Bundle만 immutable cache로 활성화한다.

## Archive integrity and signature

`checksums.txt` is ASCII and contains exactly two lexicographically ordered lines:

```text
<64 lowercase SHA-256 hex>  bundle.json
<64 lowercase SHA-256 hex>  profile.json
```

Each digest is calculated from the exact archive file bytes; trailing whitespace, an extra line, a different filename, or a
mismatch is invalid. `signature.sig` is the unpadded base64 encoding of a 64-byte Ed25519 signature over the exact
`checksums.txt` bytes. `bundle.json.signingKeyId` selects one configured trust anchor. The daemon accepts a Bundle only
when that key id is configured and its 32-byte Ed25519 public key validates the signature. There is no unsigned or
"accept any key" import mode.

The service operator supplies the trust anchors outside the Bundle archive. A key file contains a single unpadded base64
Ed25519 public key, and the daemon configuration maps a stable key id to that file. Rotating a signing key means adding a
new key id and issuing a new Bundle version; changing an existing key id's public key is not allowed.

## `bundle.json`

`bundle.json` is UTF-8 JSON, at most 256 KiB, has no duplicate or unknown fields, and is canonicalized with RFC 8785
before its catalog digest is calculated. Its schema is:

```json
{
  "schemaVersion": "taskcage.bundle/v0alpha1",
  "name": "ffmpeg-audio-to-wav",
  "version": "1.0.0",
  "signingKeyId": "taskcage-release-2026",
  "runtime": {
    "packageId": "org.taskcage.ffmpeg",
    "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
  },
  "profileDigest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
}
```

`name` and `version` use the Local Profile v2 identity rules. `signingKeyId` is 1–64 ASCII bytes matching
`[A-Za-z0-9][A-Za-z0-9._-]{0,63}`. `runtime.packageId` must equal the referenced installed Package manifest `id`;
`runtime.digest` is its immutable cache digest. `profileDigest` is the SHA-256 digest of exact `profile.json` bytes.

The following fields have these meanings:

| 항목 | 의미 |
|---|---|
| Bundle identity | name과 strict semantic version으로 구성한 불변 식별자 |
| Profile | 입력·출력 schema, argv 구성 규칙, Artifact 규칙, 기본 자원 정책 |
| Runtime reference | Runtime Package identity와 SHA-256 digest |
| Platform requirements | referenced Runtime Package의 Linux architecture, ABI, libc requirement |
| Policy | 기본 CPU·memory·PID·wall-time 제한과 허용 override 범위 |
| Provenance | license, SBOM reference, 제작자 정보와 signature |

Bundle version과 digest는 공개 후 변경하지 않는다. 계약, Runtime Package, 기본 정책 또는 platform requirement가
바뀌면 새 Bundle version을 발행한다.

## `profile.json`

`profile.json` is UTF-8 JSON, at most 256 KiB, has no duplicate or unknown fields. It describes a single Profile whose
identity must match `bundle.json`. v0alpha1 intentionally supports only one input Artifact and one output Artifact.

```json
{
  "schemaVersion": "taskcage.profile/v0alpha1",
  "name": "ffmpeg-audio-to-wav",
  "version": "1.0.0",
  "inputs": [
    {"name": "source", "kind": "LOCAL_INPUT", "required": true},
    {"name": "sample_rate_hz", "kind": "INT64", "required": true, "allowedValues": [8000, 16000, 22050, 44100, 48000]},
    {"name": "channels", "kind": "INT64", "required": true, "allowedValues": [1, 2]}
  ],
  "output": {"name": "audio", "fileName": "result.wav", "mediaType": "audio/wav", "maximumBytes": 1073741824},
  "argv": ["-i", {"input": "source"}, "-ar", {"int64": "sample_rate_hz"}, "-ac", {"int64": "channels"}, {"output": "audio"}],
  "policy": {
    "limits": {"cpuMax": {"quotaMicros": 100000, "periodMicros": 100000}, "memoryMaxBytes": 536870912, "pidsMax": 32, "wallTimeLimitMs": 120000},
    "output": {"stdoutTailMaxBytes": 65536, "stderrTailMaxBytes": 65536}
  },
  "allowedOverrides": []
}
```

`argv` never contains a program path, shell expression, environment assignment, working-directory path, glob, or string
interpolation. The daemon executes only the verified Runtime Package entrypoint. An element is either a literal string or
exactly one placeholder object: `{ "input": "<LOCAL_INPUT slot>" }`, `{ "int64": "<INT64 slot>" }`,
`{ "string": "<STRING slot>" }`, `{ "boolean": "<BOOLEAN slot>" }`, or `{ "output": "<declared output name>" }`.
Placeholder names must identify a declared slot of the matching kind. Strings are individual argv elements, not shell text.
v0alpha1 has no optional slots, caller input arrays, arbitrary caller JSON, environment, custom working directory,
multiple outputs, or caller-supplied executable.

Each `INT64` input has exactly one validation contract: either `allowedValues` or a complete `minimum`/`maximum` range.
`allowedValues` is valid only for `INT64`, contains 1 to 64 unique integers in strictly ascending canonical order, and
cannot appear with `minimum` or `maximum`. A range must contain both bounds with `minimum <= maximum`. `LOCAL_INPUT`,
`STRING`, and `BOOLEAN` inputs omit all three fields. An `INT64` request value outside its selected contract is rejected
with `INVALID_PROFILE_INPUT`, retryable `false`.

`policy` and `allowedOverrides` have the same effective-resource validation as Profile API v2. Each v0alpha1 `policy`
value is both the Profile default and the Bundle maximum. A Bundle can only reduce what the daemon deployment allows.
Unsupported profile schema or platform makes the Bundle unavailable; it does not cause a Raw Command fallback.

`policy` must contain complete `limits` and `output` objects using the Profile API v2 resource shape. `allowedOverrides`
is a unique subset of `limits.cpuMax`, `limits.memoryMaxBytes`, `limits.pidsMax`, `limits.wallTimeLimitMs`,
`output.stdoutTailMaxBytes`, and `output.stderrTailMaxBytes`. A request that supplies a field outside this set is
rejected with `LIMIT_EXCEEDS_POLICY`, retryable `false`.

An allowed override must be equal to or more restrictive than the corresponding `policy` value. CPU quota/period ratios
are compared exactly with integer cross-multiplication; memory, PID, wall time and stdout/stderr tail values must be less
than or equal to the Bundle value. The resulting effective budget is then checked independently against the daemon
deployment maximum. A Bundle or deployment maximum violation returns `LIMIT_EXCEEDS_POLICY`, retryable `false`.
An empty override or a value that does not satisfy the positive integral resource shape returns
`INVALID_PROFILE_INPUT`, retryable `false`.

All override and maximum checks finish before Artifact staging, Task or Registry records, task cgroups and target
processes are created. A rejected request leaves none of those execution side effects.

## Runtime Package 관계

Bundle은 Runtime Package digest를 항상 참조한다. daemon은 digest와 platform compatibility를 검증한
Package만 실행한다. 여러 Bundle은 하나의 Package cache entry를 공유할 수 있다.

```text
ffmpeg-runtime@sha256:...
├─ ffmpeg-transcode@1.0.0
├─ ffmpeg-thumbnail@1.0.0
└─ ffmpeg-audio-extract@1.0.0
```

작은 Runtime Package는 동일한 distribution archive에 함께 전달할 수 있다. 이 경우에도 importer는 Package를
Bundle과 분리된 digest cache entry로 검증·저장한다. 큰 Package는 local import 또는 이후 Registry에서
별도로 준비할 수 있다.

## 언어별 SDK 관계

언어별 SDK는 Capsule의 Profile schema를 해당 언어의 typed input/output API로 노출하는 선택적 편의 계층이다.
프로세스별 Binding artifact가 Capsule 실행의 필수 구성요소는 아니다.

```text
Java SDK
  → ffmpeg-transcode Capsule/Profile
  → Generic ProfileRequest
```

## Local execution

This section applies to daemon `0.5.0` and later. After importing the referenced Runtime Package and Bundle into the
same daemon-owned cache, start the daemon with the Profile Artifact root and Bundle cache root.

```text
--profile-artifact-root /var/lib/taskcage/artifacts
--profile-artifact-max-bytes 1073741824
--bundle-cache-root /var/lib/taskcage
```

For each `submitProfile`, the daemon resolves the exact installed `name@version`, revalidates the referenced Runtime
Package digest and platform, pins its entrypoint descriptor, stages the one declared input Artifact, materializes only
the declared argv placeholders, and applies the Bundle policy plus permitted request overrides. Missing, corrupt or
incompatible Bundles never fall back to a caller command or to a mutable executable path.

언어별 SDK는 Capsule의 trust boundary가 아니다. daemon은 Capsule signature, allowlist, Profile input/output
data와 resource override를 다시 검증한다.

## 사용 경로

```text
Capsule author
  → Runtime Package + Profile 제작
  → Capsule archive 생성·서명
  → local import 또는 조직 Registry 배포

Application developer
  → generic ProfileRequest 또는 language SDK 사용
  → Task 실행 결과와 output data 수신
```

## Local import and catalog

The operator first imports the referenced Runtime Package, then imports the Bundle archive using the same
service UID:

```bash
taskcaged import-package --source /srv/import/ffmpeg-7.1.1 --cache-root /var/lib/taskcage
taskcaged bundle import \
  --source /srv/import/ffmpeg-audio-to-wav-1.0.0.tcbundle.tar.gz \
  --cache-root /var/lib/taskcage \
  --trusted-key taskcage-release-2026=/etc/taskcage/bundle-keys.d/taskcage-release-2026.pub
```

Import verifies archive structure, checksums, signature, manifest/profile schema, current host platform, and the already
verified Package digest before writing anything runnable. It stages the exact verified `bundle.json` and `profile.json` under
`bundles/sha256/<bundle-digest>/` and atomically activates the identity mapping
`bundles/catalog/<name>/<version>.json`. The mapping is created under a unique `.staging-<pid>-<sequence>` name with
create-new semantics, written and file-fsynced, verified as a daemon-owned read-only regular file, then moved to the final
name with `renameat2(RENAME_NOREPLACE)`. A successful activation fsyncs the identity directory. The final mapping is
therefore either absent or a complete single-link file; activation never overwrites an existing identity.

A process exit before the rename can leave only a staging file and an unreferenced content-addressed Bundle. A process exit
after the rename but before the directory fsync can leave the complete final mapping visible without a confirmed durable
directory update. Re-import is the recovery operation: it safely reads an existing final mapping, returns
`ALREADY_PRESENT` and fsyncs the directory when the digest matches, or returns an identity conflict when it differs.
`bundle list` and `bundle inspect` ignore a stale staging file only when its generated name, regular-file type, owner,
device, link count, bounded size and read-only mode are safe. They do not automatically delete staging residue; malformed,
symlink, unexpected-type, wrong-owner or wrong-device staging entries fail closed. An occupied `(name, version)` is never
overwritten.

`taskcaged bundle list --cache-root …` returns installed identities and digests; `taskcaged bundle inspect --cache-root …
--name … --version …` returns the resolved manifest. Neither command executes a program or fetches from the network.

Hub는 이 형식의 필수 구성요소가 아니다. MVP에서는 local import와 조직의 기존 artifact 배포 경로만으로
Bundle을 제공한다. Hub는 여러 호스트·조직이 Bundle과 Runtime Package를 공유해야 한다는 실제 요구가
확인된 뒤 검토한다.

## 공개 API 전환

다음 Capsule-first 공개 계약에서는 일반 실행을 Capsule/Profile request로 제한한다. 현재 Local Raw Command는
기존 릴리스와 검증 자료를 위한 호환 경로이며, 새 public Capsule API 또는 Remote API의 일부가 아니다.
