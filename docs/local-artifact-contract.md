# Local Artifact 계약 초안

> 상태: `v0.2 Local Product Alpha`를 위한 승인된 설계 경계다. 현재 공개된 Raw Command Protocol v1의
> 동작이나 입력은 바꾸지 않는다. 이 문서는 #149의 Artifact 계약이며 Profile request/result Core는
> [Local Profile Core API v2](api-profile-v2.md)에서, Profile 실행 경로는 #150에서 연결한다.

## 범위

Artifact는 한 daemon에 설정한 **하나의 local root** 안의 regular file이다. Remote 전송, object storage,
URL, URI, Artifact service, retention API와 여러 output transaction은 이 범위에 없다.

- caller는 input Artifact만 참조하고 source file을 계속 소유한다.
- Profile은 정확히 하나의 required output slot과 고정된 output file name을 선언한다.
- daemon은 input을 실행 전에 snapshot하고, 성공한 output만 Task-scoped persistent Artifact로 공개한다.
- Raw Command는 Artifact root를 우회하거나 암묵적으로 사용하지 않는다.

## Root와 path

배포자는 canonical absolute directory 하나를 Artifact root로 설정한다. daemon은 시작 시 그 root가 symlink가
아닌 existing directory, daemon effective UID 소유, group/other non-writable이며 read, staging, publish,
cleanup 가능한지 검증한다. root 안에서 `.taskcage/`는 daemon 전용 staging subtree이고 `tasks/`는
published output subtree다.

wire path는 root 기준 상대 UTF-8 path다. 1~4,096 bytes, `/` separator만 허용하며 빈 segment, `.`, `..`,
leading/trailing slash, `\\`, NUL, ASCII control character와 첫 segment `.taskcage`를 거부한다. percent decoding,
Unicode normalization, case folding은 하지 않는다. absolute path, root selector, `file:`와 network URI도 없다.

Linux resolver는 root file descriptor 기준 `openat2`의 `RESOLVE_BENEATH`, `RESOLVE_NO_MAGICLINKS`,
`RESOLVE_NO_SYMLINKS`, `RESOLVE_NO_XDEV`와 `O_NOFOLLOW`에 동등해야 한다. input component에 symlink가 있거나
regular file이 아니면 `INVALID_ARTIFACT_PATH`로, 이를 fail-closed로 보장할 수 없으면 Product Alpha 기능을
준비 상태로 광고하지 않는다.

## Input descriptor와 snapshot

`LOCAL_INPUT` descriptor는 path, SHA-256 digest, size bytes를 모두 선언한다.

```json
{
  "kind": "LOCAL_INPUT",
  "path": "jobs/42/source.mov",
  "digest": "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
  "sizeBytes": 1048576
}
```

Task, cgroup, Registry reservation 또는 target을 만들기 전에 daemon은 descriptor-relative resolution으로 source를
열고, `.taskcage/preflight/<requestId>.<nonce>/`에 복사하면서 size와 SHA-256을 검증한다. mismatch는
`ARTIFACT_DIGEST_MISMATCH`로 거절하고 preflight copy를 제거한다. source는 daemon이 수정하거나 제거하지
않는다.

검증된 snapshot만 `.taskcage/staging/<taskId>/artifacts/in/<slot>`으로 이동한다. target은 snapshot과
Task staging output path만 보며 caller input이나 이전에 published된 file을 write 대상으로 받지 않는다.

## Output publish와 완료 의미

output은 Task staging의 `artifacts/out/<Profile fixed output fileName>`에 쓴다. 이 이름은 daemon이
Profile 계약에서 결정하며 caller가 지정할 수 없다. `terminationReason=EXITED`와 `exitCode=0`일 때만
daemon이 required output을 검사한다: symlink가 아닌 regular file, size limit, SHA-256, `fsync`, no-overwrite
atomic rename, final parent directory `fsync` 순서다.

성공 output의 유일한 공개 path는 다음과 같다.

```text
tasks/<taskId>/<Profile fixed output fileName>
```

`renameat2(RENAME_NOREPLACE)`와 동등한 no-overwrite rename이 불가능하거나 destination이 이미 있으면
`OUTPUT_PUBLISH_FAILED`로 끝낸다. 새 이름 생성이나 덮어쓰기는 성공이 아니다.

published Artifact는 다음 shape를 Profile result에 둔다.

```json
{
  "kind": "LOCAL_FILE",
  "path": "tasks/44444444-4444-4444-8444-444444444444/result.mp4",
  "digest": "sha256:5555555555555555555555555555555555555555555555555555555555555555",
  "sizeBytes": 7340032,
  "mediaType": "video/mp4"
}
```

success result는 cgroup, process, output reader **그리고 Artifact staging**의 cleanup이 확인된 뒤에만
`FINISHED`로 공개된다. non-zero exit, timeout, cancel, exec failure, output contract violation, publish failure는
final output을 만들지 않고 staging input/output/task directory를 제거한다. cleanup을 확인할 수 없으면 기존
fail-stop 계약을 적용한다. 기존 input과 기존 published output은 모든 실패 경로에서 변경하지 않는다.

daemon은 output staging에서 Profile이 선언한 fixed output file 외의 file, directory, symlink를 발견하면
`OUTPUT_CONTRACT_VIOLATION`으로 종료하고 아무 output도 publish하지 않는다.

## v0.2 이외의 비목표

- daemon 재시작 뒤 Artifact-based exactly-once 보장
- Artifact upload/download, remote transfer, object store
- caller-specified output name 또는 output overwrite
- multi-output atomic publish, mutable aliases, garbage collection
- Raw Command에 Artifact path를 추가하는 Protocol v1 변경
