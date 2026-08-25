# TaskCage 릴리스 및 버전 정책

이 문서는 TaskCage 컴포넌트의 공개 버전, Git tag와 배포 채널에 적용하는 공통 정책을 정의한다. 실제
maintainer 절차와 장애 복구 방법은 [릴리스 운영](releasing.md)을 따른다.

## 기본 원칙

- 모든 공개 릴리스는 필수 CI를 통과하고 `main` history에 포함된 커밋에서 생성한다.
- daemon과 SDK는 변경 주기와 배포 채널이 다르므로 컴포넌트별 버전을 독립적으로 관리한다.
- 컴포넌트 버전은 `MAJOR.MINOR.PATCH` 형식의 Semantic Versioning을 사용한다.
- 공개 버전에 `alpha`, `beta`, `rc`, `SNAPSHOT` 등의 qualifier를 사용하지 않는다.
- `v`는 버전의 일부가 아니라 Git tag에서 버전을 구분하는 prefix다.
- 공개된 버전, tag와 artifact는 수정하거나 재사용하지 않는다. 수정 사항은 새 버전으로 배포한다.
- Protocol 버전은 daemon과 SDK 제품 버전에서 분리한다.

## 0.x 버전

`0.x`는 공개 API와 운영 계약을 검증하는 초기 개발 버전이다. `0.x`에서도 가능한 범위에서 하위 호환성을
유지하지만, 호환성을 깨는 변경이 minor 버전에 포함될 수 있다. `1.0.0`부터 안정적인 공개 API와 호환성
정책을 적용한다.

공개 버전 qualifier는 사용하지 않지만 `0.x` GitHub Release는 프로젝트 성숙도를 알리기 위해
prerelease로 표시한다.

버전 증가는 다음 기준을 사용한다.

| 변경 | `0.x` 기준 | `1.x` 이후 |
|---|---|---|
| 호환되는 버그·보안 수정 | PATCH | PATCH |
| 호환되는 기능 추가 | MINOR | MINOR |
| 호환성을 깨는 공개 계약 변경 | MINOR | MAJOR |
| 문서·CI만 변경하고 배포물이 동일함 | 새 버전 없음 | 새 버전 없음 |

## 컴포넌트와 tag

공개 tag는 `<component>-v<version>` 형식을 사용한다.

| 컴포넌트 | tag 예시 | 내부 버전 위치 | 배포처 |
|---|---|---|---|
| TaskCage daemon | `taskcaged-v0.5.0` | `daemon/Cargo.toml` | GitHub Release |
| TaskCage CLI | `taskcage-v0.5.0` | `daemon/Cargo.toml` (daemon과 공유) | GitHub Release |
| Java SDK | `java-sdk-v0.4.0` | `java-sdk/build.gradle.kts` | Maven Central, GitHub Release |
| Python SDK | `python-sdk-v0.1.0` | 해당 package manifest | PyPI, GitHub Release |
| JavaScript SDK | `javascript-sdk-v0.1.0` | 해당 package manifest | npm, GitHub Release |
| Go SDK | `go-sdk-v0.1.0` | Go module release metadata | Go Modules, GitHub Release |

`taskcage` CLI와 `taskcaged` daemon은 같은 Rust package에서 빌드되므로 source version은 공유한다. 다만
Capsule 작성자는 daemon installer가 필요하지 않으므로 CLI는 별도 tag·Release archive로 배포한다. 아직 구현되지
않은 SDK의 tag와 배포 workflow는 생성하지 않는다. tag의 버전은 해당 컴포넌트 내부 버전과 정확히 일치해야 하며
CI가 불일치를 거부한다.

tag는 maintainer가 검증한 `main` 커밋에 서명해 생성한다. 컴포넌트 tag가 해당 컴포넌트의 build, test와
배포 workflow를 시작한다. `taskcage` CLI는 shared Rust package의 version만 daemon과 공유하며, daemon archive나
installer를 함께 배포하지 않는다.

## 개발 빌드

- PR과 `main` 빌드는 공개 package registry에 배포하지 않는다.
- 검증용 GitHub Actions artifact는 commit SHA와 workflow run으로 식별한다.
- 로컬 검증은 Maven Local 등 공개되지 않는 저장소를 사용할 수 있다.
- 다음 공개 버전은 manifest에서 명시적으로 변경하고, 그 변경이 병합된 뒤 tag를 생성한다.

`SNAPSHOT`을 사용하지 않는다는 정책은 개발 build를 공개 release처럼 배포하지 않는다는 의미다. commit
artifact를 Maven Central, PyPI, npm 또는 안정 배포 채널에 올리지 않는다.

## Protocol 호환성

제품 버전과 Protocol 버전은 같은 의미가 아니다.

```text
taskcaged 0.1.0   ─┐
taskcaged 0.5.0   ─┼─ Local Protocol v1, v2; Remote Protocol v1
Java SDK 0.4.0    ─┘
```

daemon과 SDK는 제품 버전 문자열이 아니라 서로 지원하는 Protocol version의 교집합으로 연결 가능 여부를
판정한다. Protocol field, 상태, 오류 코드 또는 의미를 변경하면 API 명세, daemon, SDK와 fixture를 같은
변경에서 갱신한다.

각 컴포넌트 릴리스 노트에는 지원하는 Protocol version과 필요한 최소 상대 컴포넌트 버전을 기록한다.

## SDK 배포

각 SDK는 해당 언어의 표준 package registry에 독립적으로 배포한다. GitHub Release는 변경 내역, 지원
Protocol과 source tag를 제공하고, 실제 dependency 설치는 표준 registry를 사용한다.

Java Core SDK의 Maven 좌표는 다음 형식을 유지한다.

```text
org.taskcage:taskcage-java-sdk:<version>
```

Maven Central에 공개된 component는 덮어쓰지 않는다. Central deployment가 `PUBLISHED`가 된 것을 확인한
뒤 같은 tag의 GitHub Release를 공개한다.

## Daemon 배포 단계

daemon은 설치와 upgrade 계약이 검증된 정도에 따라 배포 채널을 확장한다.

### 1단계: GitHub Release archive

- Linux release binary
- Ubuntu installer와 uninstaller
- systemd unit과 기본 설정
- SHA-256 checksum

이 단계에서는 실제 사용자가 설치·기동·재설치·rollback·제거할 수 있는지 검증한다.

### 2단계: GitHub Release Debian package

검증된 archive 설치 계약을 Debian package lifecycle로 옮긴다. 최초 설치, 재설치, upgrade, 설정 보존,
service restart, remove와 purge를 검증한 뒤 같은 daemon GitHub Release에 `.deb`를 추가한다.

TaskCage 버전과 Debian package revision은 분리한다.

```text
TaskCage version:       0.1.0
Debian package version: 0.1.0-1
```

### 3단계: 서명된 APT repository

둘 이상의 `.deb` 버전 사이 upgrade와 rollback이 검증되고 자동 업데이트가 필요한 실제 사용자가 생기면
서명된 TaskCage APT repository를 제공한다.

```bash
sudo apt update
sudo apt install taskcaged
```

GitHub Release와 APT repository에는 한 번 생성한 동일한 `.deb`를 게시하며 같은 버전을 별도로 다시 build하지
않는다.

## 릴리스 흐름

```text
작업 브랜치 → PR → main → 필수 CI 통과
                         ↓
                  컴포넌트 버전 확인
                         ↓
                    서명된 tag
                         ↓
             컴포넌트별 build·검증
                         ↓
              비공개 deployment/draft
                         ↓
                   maintainer 승인
                         ↓
              registry와 GitHub 공개
```

외부 registry 업로드와 GitHub Release 공개는 재시도 가능한 단계로 분리한다. 공개 전에는 draft 또는
`USER_MANAGED` deployment를 사용하고, 공개된 artifact를 수정하는 대신 새 patch 또는 minor 버전을
발행한다.

## 릴리스 노트

각 GitHub Release는 최소한 다음 정보를 포함한다.

- 사용자에게 영향을 주는 기능과 수정
- 호환성을 깨는 변경과 migration 방법
- 지원 OS·architecture 또는 Java version
- 지원 Protocol version과 상대 컴포넌트 호환 범위
- 설치 또는 dependency 좌표
- 알려진 제한과 보안 경계
- 이전 버전에서 변경된 PR과 contributor

릴리스 자동화에 필요한 deployment ID나 내부 상태만으로 공개 릴리스 노트를 구성하지 않는다.
