# TaskCage 릴리스 운영

이 문서는 maintainer가 TaskCage daemon, Java Core SDK와 공식 Java Binding을 검증·배포하고 실패를 복구하는 절차를 정의한다.
공개 버전과 tag 규칙은 [릴리스 및 버전 정책](release-policy.md)을 따른다.

첫 컴포넌트 버전 `0.1.0`은 GitHub prerelease와 Maven Central에 공개됐다. 공개된 tag와 artifact는
수정하거나 재사용하지 않는다. 아래 명령은 현재 준비 중인 컴포넌트 버전을 기준으로 하며, 후속 릴리스는
manifest를 새 patch 또는 minor 버전으로 변경하고 모든 예시의 버전을 함께 바꾼다.

## 컴포넌트 계약

| 컴포넌트 | tag | 배포 workflow | 공개 위치 |
|---|---|---|---|
| daemon | `taskcaged-v0.4.0` | `Release taskcaged` | GitHub Release |
| Java Core SDK | `java-sdk-v0.3.0` | `Release Java SDK` | Maven Central, GitHub Release |
| FFmpeg Java Binding | `ffmpeg-binding-v0.1.0` | `Release FFmpeg Binding` | Maven Central, GitHub Release |

컴포넌트는 독립적으로 배포하며 제품 버전이 같을 필요가 없다. Core SDK `0.3.0`은 Local Protocol v1·v2와
Remote Protocol v1을 지원하며 Remote Profile 실행에는 daemon `0.4.0` 이상이 필요하다. FFmpeg Binding
`0.1.0`은 Core SDK `0.2.0`과 `ffmpeg-audio-to-wav@1.0.0` Profile을 제공하는 daemon `0.2.0`을 요구한다.
연결 호환성은 제품 버전 문자열이 아니라 공통 Protocol version으로 판단한다.

## 공개 산출물

daemon GitHub prerelease에는 다음 파일을 게시한다.

```text
taskcage-v0.4.0-x86_64-unknown-linux-gnu.tar.gz
taskcage-v0.4.0-x86_64-unknown-linux-gnu.tar.gz.sha256
taskcage-v0.4.0-aarch64-unknown-linux-gnu.tar.gz
taskcage-v0.4.0-aarch64-unknown-linux-gnu.tar.gz.sha256
install-taskcaged.sh
```

archive는 `bin/taskcaged`, Ubuntu installer·uninstaller, systemd unit, 기본 설정, README와 LICENSE를
versioned top-level directory 아래에 포함한다.

Java SDK는 Maven Central에 다음 좌표로 main JAR, sources JAR, Javadoc JAR와 POM을 게시한다. 각 파일에는
PGP signature와 Maven Central이 요구하는 checksum을 포함한다.

```text
org.taskcage:taskcage-java-sdk:0.3.0
org.taskcage:taskcage-ffmpeg-binding:0.1.0
```

Java SDK GitHub prerelease는 사용자 변경 내역, Maven 좌표, 지원 Java와 Protocol version을 제공한다.
Central upload zip은 Portal 입력과 복구용 CI artifact이며 사용자 설치 파일이 아니다.

## GitHub 설정 사전 조건

### 공통

1. `main` branch protection과 필수 CI를 설정한다.
2. `taskcaged-release`와 `java-sdk-release` environment를 만든다.
3. 두 environment에 required reviewer를 지정한다.
4. Actions가 tag의 GitHub signature verification 결과와 `main` history 포함 여부를 확인할 수 있도록 기본
   `GITHUB_TOKEN`의 contents read 권한을 유지한다.

`taskcaged-release` environment에는 별도 secret이 필요하지 않다. maintainer가 daemon Draft Release와
checksum을 검토한 뒤 수동 publish workflow를 실행하면 이 environment의 승인에서 대기한다.

### Java SDK

Central Portal에서 `taskcage.org` 도메인 소유권을 사용해 `org.taskcage` namespace ownership을 검증하고 `java-sdk-release` environment에
다음 secret을 등록한다.

| secret | 값 |
|---|---|
| `CENTRAL_USERNAME` | Central Portal publishing token username |
| `CENTRAL_PASSWORD` | Central Portal publishing token password |
| `MAVEN_SIGNING_KEY` | ASCII-armored private PGP signing key |
| `MAVEN_SIGNING_PASSWORD` | private key passphrase |

private key와 Central token을 repository, log, release note 또는 장기 보존 artifact에 기록하지 않는다. PR
CI는 release secret을 받지 않고 매번 폐기되는 임시 PGP key로 Central bundle 구조만 검증한다.

## tag 전 release candidate 검증

일반 PR CI는 변경된 범위에 맞는 검사만 실행한다. tag를 만들기 전 maintainer는 Actions의
`Validate release candidate` workflow를 수동 실행하고 daemon version을 입력한다. 이 workflow는
각 컴포넌트의 현재 manifest 버전으로 다음을 수행한다.

- Linux x86-64와 ARM64 containerized Java E2E
- x86-64 daemon archive 및 SHA-256 sidecar 생성과 installer smoke test
- 임시 PGP key로 Java Core SDK와 FFmpeg Binding Central validation bundle 생성
- 두 bundle만 제공하는 임시 Maven repository에서 별도 소비자 project compile
- daemon version과 각 컴포넌트 manifest version 일치 검사
- archive의 prebuilt binary와 installer를 사용한 Ubuntu systemd smoke test
- 검토용 GitHub Actions artifact 업로드

이 artifact는 공개 릴리스가 아니며 Maven Central이나 GitHub Release에 복사해 게시하지 않는다. 실제
릴리스 workflow가 tag commit에서 산출물을 다시 생성한다. tag workflow는 이 사전 검증을 대체하지
않지만, 서명된 tag와 `main` 포함 여부를 포함한 최종 배포 gate로 계속 동작한다.

## daemon 릴리스

### 1. tag 생성

daemon manifest version을 확인하고 모든 필수 CI가 통과한 `main` 커밋에 서명된 tag를 만든다.

```bash
git switch main
git pull --ff-only
bash scripts/release/verify-version.sh taskcaged 0.4.0
git tag --sign taskcaged-v0.4.0 -m "TaskCage daemon 0.4.0"
git push origin taskcaged-v0.4.0
```

tag push는 `.github/workflows/release-daemon.yml`을 시작한다. workflow는 다음을 확인한다.

- tag가 `taskcaged-vMAJOR.MINOR.PATCH` 형식임
- tag version과 `daemon/Cargo.toml`이 일치함
- GitHub가 tag signature를 verified로 판정함
- tag commit이 `origin/main` history에 포함됨
- stable Rust의 format, clippy와 전체 test
- 선언된 최소 Rust 버전의 locked dependency 전체 test
- cgroup preflight·runner, systemd와 FFmpeg compatibility gate

### 2. draft 검토와 공개

검증이 끝나면 workflow가 daemon archive, checksum과 bootstrap installer를 생성해 Draft GitHub prerelease에
첨부한다.

승인 전에 maintainer는 다음을 확인한다.

- archive와 checksum 파일명이 tag version과 일치하고 bootstrap installer가 함께 첨부됨
- Actions artifact와 Draft asset의 checksum이 일치함
- 릴리스 노트에 사용자 변경, 지원 플랫폼, Protocol과 알려진 제한이 포함됨
- 깨끗한 Ubuntu 24.04 x86-64와 ARM64 환경에서 각 archive smoke test가 통과함

검토가 끝나면 GitHub Actions의 `Release taskcaged` workflow를 `tag=taskcaged-v0.4.0`으로 수동
실행한다. `taskcaged-release` environment를 승인하면 workflow는 기존 Draft가 prerelease인지
확인하고 공개한다. Draft 생성 뒤 workflow가 중단돼도 같은 tag로 공개 단계만 다시 실행할 수 있다.

## Java SDK 릴리스

### 1. tag 생성과 prepare

Java SDK manifest version을 확인하고 `main`의 검증된 커밋에 독립 tag를 만든다.

```bash
git switch main
git pull --ff-only
bash scripts/release/verify-version.sh java-sdk 0.3.0
git tag --sign java-sdk-v0.3.0 -m "TaskCage Java SDK 0.3.0"
git push origin java-sdk-v0.3.0
```

tag push는 `.github/workflows/release-java-sdk.yml`의 prepare 경로를 시작한다. workflow는 tag signature,
manifest version과 `main` 포함 여부를 검증하고 Java build와 실제 daemon·FFmpeg compatibility gate를
수행한다.

`java-sdk-release` environment를 승인하면 다음 작업을 수행한다.

1. release PGP key로 Central bundle을 생성한다.
2. 같은 Maven coordinate가 아직 공개되지 않았는지 확인한다.
3. Central에 `USER_MANAGED` deployment를 업로드한다.
4. deployment ID를 포함한 Draft GitHub prerelease를 생성한다.
5. 서명된 Central bundle을 30일 보존 Actions artifact로 저장한다.

이 시점에는 Maven Central과 GitHub Release 모두 공개되지 않는다.

### 2. validation과 finalize

Central deployment가 `VALIDATED`인지 확인하고 Draft release의 사용자용 내용을 완성한다. GitHub Actions의
`Release Java SDK` workflow를 수동 실행한다.

```text
tag: java-sdk-v0.3.0
central_deployment_id: Draft release에 기록된 UUID
```

`java-sdk-release` environment를 다시 승인하면 finalize가 다음 순서로 동작한다.

1. 입력 deployment ID가 해당 Draft release body에 결합돼 있는지 확인한다.
2. Central 상태가 `VALIDATED`이면 publish를 요청한다.
3. Central 상태가 `PUBLISHED`가 될 때까지 유한한 시간 동안 확인한다.
4. Central 공개가 확인된 뒤에만 GitHub prerelease를 공개한다.

이미 Central은 `PUBLISHED`이고 GitHub Release만 Draft라면 같은 tag와 deployment ID로 finalize를 다시
실행할 수 있다.

## FFmpeg Binding 릴리스

Java Core SDK `0.2.0`과 taskcaged `0.2.0`이 모두 공개된 뒤 검증된 `main` 커밋에 Binding tag를 생성한다.

```bash
git switch main
git pull --ff-only
bash scripts/release/verify-version.sh ffmpeg-binding 0.1.0
git tag --sign ffmpeg-binding-v0.1.0 -m "TaskCage FFmpeg Binding 0.1.0"
git push origin ffmpeg-binding-v0.1.0
```

`Release FFmpeg Binding` workflow는 Core SDK `0.2.0` POM과 공개 taskcaged `0.2.0` prerelease를 먼저
확인한다. 이후 Binding 단위 build, 실제 container daemon E2E, 서명된 Central bundle 생성과
`USER_MANAGED` upload를 수행하고 Draft GitHub prerelease를 만든다. Central deployment가 `VALIDATED`가
되면 같은 workflow를 다음 입력으로 수동 실행해 Central과 GitHub prerelease를 순서대로 공개한다.

```text
tag: ffmpeg-binding-v0.1.0
central_deployment_id: Draft release에 기록된 UUID
```

## 설치 확인

daemon은 [Ubuntu daemon 설치](install-ubuntu.md)의 checksum-first 절차로 설치한다. 설치 후 같은 service
UID에서 live status를 호출하고 `daemonVersion`이 archive version과 같은지 확인한다.

Java SDK는 Central 상태가 `PUBLISHED`가 되고 실제 POM URL이 resolve되는지 확인한 뒤 사용한다.

```kotlin
dependencies {
    implementation("org.taskcage:taskcage-java-sdk:0.2.0")
    implementation("org.taskcage:taskcage-ffmpeg-binding:0.1.0")
}
```

## 실패, 재시도와 응답 유실

### 공통

- tag validation이나 build 실패: 공개 상태가 없으므로 원인을 수정한 새 commit과 새 버전을 사용한다.
- tag는 이미 외부에 공유된 버전 식별자이므로 다른 commit을 가리키도록 강제로 이동하지 않는다.
- 공개 GitHub Release와 registry artifact는 덮어쓰지 않는다. 결함은 새 patch 또는 minor 버전으로 수정한다.
- 같은 workflow를 재실행하기 전에 Draft Release, Central deployment와 공개 registry 상태를 먼저 확인한다.

### daemon

- Draft 생성 전 실패: 같은 workflow run을 재시도할 수 있다.
- Draft 생성 응답 유실: tag의 Draft Release가 존재하는지 먼저 확인한다. 기존 Draft가 있으면 자산과
  checksum을 검토하고 publish 단계만 복구한다.
- 공개 뒤 결함 발견: Release에 경고를 추가하고 이전 archive와 checksum으로 rollback한 뒤 새 버전을
  발행한다.

### Java SDK

- Central upload 전 실패: 외부 deployment가 없으므로 prepare를 재시도할 수 있다.
- upload 응답 유실 또는 upload 뒤 Draft 생성 실패: Central Portal에서 deployment name과 coordinate를
  조회한다. 기존 deployment를 찾으면 ID를 보존해 Draft와 연결하고, 사용하지 않을 deployment는 공개 전에
  drop한다. 확인 없이 같은 coordinate를 다시 upload하지 않는다.
- Central validation 실패: validation error를 수정하고 새 버전을 발행한다. 실패한 bundle을 강제로
  publish하지 않는다.
- finalize 응답 유실: Central 상태를 먼저 확인한다. `PUBLISHED`이면 같은 deployment ID로 finalize를 다시
  실행해 Draft만 공개한다. `VALIDATED`이면 publish 요청을 재개한다.
- Central에 공개된 뒤 결함 발견: artifact를 삭제·교체하지 않고 새 Java SDK 버전을 배포한다.

daemon rollback은 이전 archive와 checksum을 보존하고 installer에 이전 binary를 다시 전달한다. installer는
기존 `/etc/taskcage/taskcaged.env`를 덮어쓰지 않으므로 설정 호환성이 깨졌다면 이전 설정도 함께 복원한다.

## 구현 근거

- [Central Portal Gradle 안내](https://central.sonatype.org/publish/publish-portal-gradle/)
- [Central Portal Publisher API](https://central.sonatype.org/publish/publish-portal-api/)
- [Central upload와 deployment 상태](https://central.sonatype.org/publish/publish-portal-upload/)
- [Maven Central component 요구사항](https://central.sonatype.org/publish/requirements/)
- [Maven Central immutability](https://central.sonatype.org/publish/requirements/immutability/)
- [Gradle Maven Publish Plugin](https://docs.gradle.org/current/userguide/publishing_maven.html)
- [Gradle Signing Plugin](https://docs.gradle.org/current/userguide/publishing_signing.html)
