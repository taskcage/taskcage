# TaskCage Bundle 형식 초안

> 상태: **계획 초안**. 이 문서는 아직 daemon, SDK 또는 Protocol의 구현 계약이 아니다. Local Bundle import와
> Bundle-first 공개 API를 구현하기 전에 합의할 최소 배포 모델을 정의한다.

## 목적

TaskCage Bundle은 특정 외부 프로그램 작업을 실행하기 위한 불변 계약이다. Bundle은 실행 파일 경로나
caller-provided argv를 노출하지 않고, 어떤 Profile을 어떤 Runtime Package와 정책으로 실행할지 정의한다.

```text
Bundle = what and how to execute
Runtime Package = executable files and dependencies
Task = one cgroup-managed execution of a Bundle Profile
```

Bundle은 Docker Image가 아니다. root filesystem, PID 1, namespace, network configuration, container user와
Task별 input/output data를 포함하지 않는다.

## 배포 archive

초기 Bundle 배포물의 권장 이름은 다음과 같다.

```text
<bundle-name>-<bundle-version>.tcbundle.tar.gz
```

archive는 최소한 다음 파일을 가진다.

```text
bundle.json
profile.json
checksums.txt
signature.sig
```

archive reader는 압축 해제 전에 크기·file count 상한을 적용하고, absolute path, `..` path traversal, symlink,
hardlink, device, FIFO, socket과 중복 경로를 거부해야 한다. daemon은 검증된 staging directory에서만 archive를
처리하고, 검증이 끝난 Bundle만 immutable cache로 활성화한다.

## `bundle.json`의 최소 의미

구체 JSON schema는 후속 구현에서 고정한다. 다만 Bundle에는 다음 의미가 반드시 있어야 한다.

| 항목 | 의미 |
|---|---|
| Bundle identity | name과 strict semantic version으로 구성한 불변 식별자 |
| Profile | 입력·출력 schema, argv 구성 규칙, Artifact 규칙, 기본 자원 정책 |
| Runtime reference | Runtime Package identity와 SHA-256 digest |
| Platform requirements | Linux architecture, ABI, libc, 선택적 hardware requirement |
| Policy | 기본 CPU·memory·PID·wall-time 제한과 허용 override 범위 |
| Provenance | license, SBOM reference, 제작자 정보와 signature |

Bundle version과 digest는 공개 후 변경하지 않는다. 계약, Runtime Package, 기본 정책 또는 platform requirement가
바뀌면 새 Bundle version을 발행한다.

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

## Binding 관계

Binding은 Bundle/Profile schema를 특정 언어의 도메인 API로 매핑하는 선택적 library다.

```text
Java FFmpeg Binding
  → ffmpeg-transcode Bundle/Profile
  → Generic ProfileRequest
```

Binding은 Bundle의 trust boundary가 아니다. daemon은 Bundle signature, allowlist, Profile input, Artifact와
resource override를 다시 검증한다. Binding은 지원하는 Bundle/Profile version 범위와 필요한 Core SDK·Protocol
version을 공개해야 한다.

## 사용 경로

```text
Bundle author
  → Runtime Package + Profile 제작
  → Bundle archive 생성·서명
  → local import 또는 조직 Registry 배포

Application developer
  → generic ProfileRequest 또는 language Binding 사용
  → Task 결과와 output Artifact 수신
```

Hub는 이 형식의 필수 구성요소가 아니다. MVP에서는 local import와 조직의 기존 artifact 배포 경로만으로
Bundle을 제공한다. Hub는 여러 호스트·조직이 Bundle과 Runtime Package를 공유해야 한다는 실제 요구가
확인된 뒤 검토한다.

## 공개 API 전환

다음 Bundle-first 공개 계약에서는 일반 실행을 Bundle/Profile request로 제한한다. 현재 Local Raw Command는
기존 릴리스와 검증 자료를 위한 호환 경로이며, 새 public Bundle API 또는 Remote API의 일부가 아니다.
