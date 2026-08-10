# Public Alpha release 운영

이 문서는 TaskCage Local Public Alpha의 release 산출물, 승인 경계와 장애 복구 절차를 정의한다.
현재 준비 버전은 `0.1.0-alpha.1`이며 이 pipeline을 merge하는 것만으로 tag, GitHub Release 또는 Maven
Central artifact가 생성되지는 않는다.

## Release 계약

daemon과 Java SDK는 v0.x 동안 같은 release train을 사용한다.

| 항목 | `0.1.0-alpha.1` 계약 |
|---|---|
| Git tag | `v0.1.0-alpha.1` |
| daemon package version | `0.1.0-alpha.1` |
| Java SDK version | `0.1.0-alpha.1` |
| Maven 좌표 | `io.github.taskcage:taskcage-java-sdk:0.1.0-alpha.1` |
| daemon target | Linux x86-64, `x86_64-unknown-linux-gnu` |
| Protocol | v1 |

`scripts/release/verify-version.sh`는 tag, Rust manifest와 Gradle version이 정확히 일치하는지 검사한다.
`SNAPSHOT`이나 서로 다른 버전은 release할 수 없다. workflow는 tag가 `main` history에 포함되는지도 확인한다.

## 공개 산출물

GitHub prerelease에는 다음 두 파일을 게시한다.

```text
taskcage-v0.1.0-alpha.1-x86_64-unknown-linux-gnu.tar.gz
taskcage-v0.1.0-alpha.1-x86_64-unknown-linux-gnu.tar.gz.sha256
```

archive는 하나의 versioned top-level directory만 가지며 다음 자산을 포함한다.

```text
bin/taskcaged
packaging/ubuntu/install-taskcaged.sh
packaging/ubuntu/uninstall-taskcaged.sh
packaging/ubuntu/taskcaged.env
packaging/ubuntu/taskcaged.service
README.md
LICENSE
```

Java SDK는 Maven Central에 main JAR, sources JAR, Javadoc JAR와 POM을 배포한다. 각 파일에는 PGP
signature와 Maven Central이 요구하는 checksum이 포함된다. Central upload zip은 공개 설치 자산이 아니라
Portal이 Maven repository layout을 검증하고 게시하기 위한 입력이다.

## GitHub 설정 사전 조건

release를 실행하기 전에 maintainer가 다음 설정을 완료해야 한다.

1. Central Portal에서 `io.github.taskcage` namespace ownership을 검증한다.
2. GitHub repository에 `public-alpha-release` environment를 만든다.
3. environment에 required reviewer를 지정해 실제 Central/GitHub 공개 전에 사람의 승인을 요구한다.
4. environment secret을 등록한다.

| secret | 값 |
|---|---|
| `CENTRAL_USERNAME` | Central Portal에서 발급한 publishing token username |
| `CENTRAL_PASSWORD` | 같은 publishing token password |
| `MAVEN_SIGNING_KEY` | 공개 가능한 release key가 아닌 ASCII-armored private PGP key |
| `MAVEN_SIGNING_PASSWORD` | private key passphrase |

private key와 Central token을 repository, artifact, log 또는 release note에 넣지 않는다. PR CI는 이 secret을
받지 않으며 매번 폐기되는 임시 PGP key로 bundle 구조만 검증한다.

## PR candidate 검증

일반 CI의 `release-candidate` job은 다음을 수행한다.

- Linux x86-64 daemon archive와 SHA-256 sidecar 생성
- 임시 PGP key로 Maven Central 형식의 검증 전용 bundle 생성
- archive에서 꺼낸 binary와 installer를 이용한 실제 Ubuntu systemd smoke test
- 검토용 Actions artifact 업로드

이 Actions artifact는 provenance가 확인된 release가 아니며 Maven Central에 업로드하거나 사용자에게
배포해서는 안 된다. 실제 cgroup·systemd 조건이 없어서 test가 종료 코드 77을 반환하면 pass가 아니다.

## 실제 release 절차

`.github/workflows/release.yml`은 수동 실행만 허용하며 `prepare`와 `finalize`를 분리한다.

### 1. 태그 준비

release commit이 `main`에 merge되고 모든 필수 CI가 통과한 뒤 maintainer가 검토한 commit에 서명된 tag를
만들어 push한다. 이 문서 변경을 검토하는 동안에는 tag를 만들지 않는다.

```bash
git switch main
git pull --ff-only
git tag --sign v0.1.0-alpha.1 -m "TaskCage v0.1.0-alpha.1"
git push origin v0.1.0-alpha.1
```

### 2. prepare

Actions의 `Public Alpha release` workflow를 `mode=prepare`, `tag=v0.1.0-alpha.1`로 실행한다. prepare는
Rust·Java, 실제 cgroup, systemd와 FFmpeg release gate를 모두 다시 수행한다. 성공한 경우:

1. daemon archive와 checksum을 만든다.
2. release PGP key로 Java Central bundle을 만든다.
3. Maven coordinate가 아직 공개되지 않았는지 확인한다.
4. bundle을 Central에 `USER_MANAGED`로 업로드한다.
5. Central deployment ID를 포함한 Draft GitHub prerelease를 만든다.

`USER_MANAGED` upload는 이 시점에 Maven artifact를 공개하지 않는다. deployment ID와 Central의 validation
결과, archive checksum, Draft release 자산을 maintainer가 확인한다.

### 3. finalize

Central deployment가 `VALIDATED`인 것을 확인한 뒤 같은 workflow를 `mode=finalize`, 같은 tag와 Draft
release note에 기록된 `central_deployment_id`로 실행한다. finalize는 ID가 해당 Draft에 결합돼 있는지
확인하고 Central publish를 요청한다. Central 상태가 `PUBLISHED`가 되기 전에는 GitHub release를 공개하지
않는다. 이미 `PUBLISHED`이고 GitHub release만 Draft라면 같은 finalize 입력으로 다시 실행할 수 있다.

공개된 release는 prerelease로 유지한다. Maven Central artifact와 공개 GitHub Release 자산은 같은 버전으로
덮어쓰지 않는다. 수정이 필요하면 원인을 고친 뒤 다음 버전을 발행한다.

## 설치 확인

daemon 설치자는 [Ubuntu daemon 설치](install-ubuntu.md)의 checksum-first 절차를 사용한다. 설치 뒤에는
같은 service UID로 live status를 확인하고 `daemonVersion`이 다운로드한 버전과 정확히 같은지 검사한다.

Maven Central 공개가 확인된 뒤 Java 사용자는 다음 좌표를 추가한다.

```kotlin
dependencies {
    implementation("io.github.taskcage:taskcage-java-sdk:0.1.0-alpha.1")
}
```

Central 공개 전에는 이 좌표가 resolve되지 않는 것이 정상이다.

## 실패, 재시도와 응답 유실

외부 시스템을 변경하는 단계는 성공 여부를 확인하기 전까지 무작정 재실행하지 않는다.

- Central upload 전 prepare 실패: 외부 배포 상태가 없으므로 원인을 고친 뒤 다시 실행할 수 있다.
- upload 응답 유실 또는 upload 성공 뒤 Draft 생성 실패: Central Portal에서 deployment name과 좌표를 먼저
  조회한다. 기존 deployment를 찾으면 ID를 보존해 Draft와 연결하고, 재사용하지 않을 경우 공개 전에
  Portal에서 drop한다. 확인 없이 같은 좌표를 다시 upload하지 않는다.
- Central validation 실패: Portal validation error를 고친다. 같은 bundle을 억지로 publish하지 않는다.
- finalize 응답 유실: Central 상태를 먼저 조회한다. `PUBLISHED`이면 같은 deployment ID로 finalize를 다시
  실행해 Draft만 공개할 수 있다. `VALIDATED`이면 publish 요청을 재개한다.
- GitHub release가 이미 공개됐는데 Central이 `PUBLISHED`가 아니면 workflow는 fail closed한다. maintainer가
  GitHub release를 Draft로 되돌리고 Central 상태를 복구할 때까지 새 release를 진행하지 않는다.
- Central에 공개된 뒤 결함 발견: Central artifact는 immutable하므로 삭제·교체를 시도하지 않는다. GitHub
  prerelease에 경고를 추가하고 설치 문서의 이전 검증 버전으로 rollback한 뒤 새 patch prerelease를 만든다.

daemon rollback은 이전 archive와 checksum을 보존하고 installer에 이전 binary를 다시 전달한다. installer는
기존 `/etc/taskcage/taskcaged.env`를 덮어쓰지 않으므로 설정 호환성이 깨진 경우 이전 설정도 함께 복원한다.

## 구현 근거

- [Central Portal Gradle 안내](https://central.sonatype.org/publish/publish-portal-gradle/)
- [Central Portal Publisher API](https://central.sonatype.org/publish/publish-portal-api/)
- [Central upload와 deployment 상태](https://central.sonatype.org/publish/publish-portal-upload/)
- [Maven Central component 요구사항](https://central.sonatype.org/publish/requirements/)
- [Maven Central immutability](https://central.sonatype.org/publish/requirements/immutability/)
- [Gradle Maven Publish Plugin](https://docs.gradle.org/current/userguide/publishing_maven.html)
- [Gradle Signing Plugin](https://docs.gradle.org/current/userguide/publishing_signing.html)
