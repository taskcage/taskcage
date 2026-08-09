# Contributing to TaskCage

TaskCage에 기여해 주셔서 감사합니다. 이 문서는 두 명의 초기 기여자가 빠르고 예측 가능하게 협업하기 위한 최소 규칙입니다. 프로젝트가 성장하면 실제 불편을 기준으로 보완합니다.

## 시작하기

- 작업을 시작하기 전에 기존 Issue가 있는지 확인합니다. 없으면 해결하려는 문제와 완료 기준을 간단히 적은 Issue를 만듭니다.
- 큰 구현은 작은 단위의 Issue와 PR로 나눕니다.
- `main` 브랜치에는 직접 push하지 않습니다.

## 브랜치 이름

기본 형식은 다음과 같습니다.

```text
<type>/<short-description>
```

사용하는 type은 다음과 같습니다.

- `feat`: 새 기능
- `fix`: 버그 또는 회귀 수정
- `refactor`: 동작 변경 없는 구조 개선
- `docs`: 문서 변경
- `test`: 테스트 추가 또는 수정
- `chore`: 빌드, 설정, 운영 보조 변경
- `hotfix`: 긴급 수정

예시:

```text
feat/cgroup-runner
fix/cleanup-timeout-processes
docs/contributing-guide
test/memory-limit-fixture
```

## 커밋 메시지

기본 형식은 다음과 같습니다.

```text
<type>(<scope>): <summary>
```

scope가 명확하지 않으면 생략할 수 있습니다.

```text
<type>: <summary>
```

브랜치 type과 같은 `feat`, `fix`, `refactor`, `docs`, `test`, `chore`를 사용합니다. summary는 영어 소문자 명령형에 가깝게 짧게 쓰고, 한 커밋에는 하나의 의도만 담습니다.

예시:

```text
feat(cgroup): add per-task memory limit
fix(runner): reap child after timeout
docs: add contribution guide
test(protocol): cover invalid frame length
```

## Pull Request

- PR 제목은 커밋 메시지 형식을 따릅니다.
- 하나의 PR은 하나의 문제를 해결하며, 리뷰 가능한 크기로 유지합니다.
- `main`에 병합하려면 다른 기여자 한 명의 승인을 받습니다.
- PR 본문에는 아래 내용을 모두 작성합니다. 해당 내용이 없으면 `None`이라고 적습니다.

```md
## Summary

## Problem

## Solution

## Changes

## Verification

## Related Issues
```

`Verification`에는 실행한 테스트와 확인한 Linux 환경을 적습니다. cgroup·프로세스 정리처럼 운영 위험이 있는 변경은 테스트, 로그, 메트릭, 재현 절차 중 적어도 하나의 검증 근거를 남깁니다.

## Rust 품질 검사

Rust 워크스페이스가 준비된 뒤 PR 전에는 아래 명령을 실행합니다.

```bash
cargo fmt --check
cargo clippy -- -D warnings
cargo test
```

포맷 자동 적용은 `cargo fmt`를 사용합니다. 대규모 포맷 변경은 기능 변경과 별도 PR로 분리합니다.

## 설계와 문서

- README는 외부 사용자를 위한 현재 기능, 제약과 시작점만 간결하게 유지합니다.
- 구현된 계약과 향후 후보를 명확히 구분하고, 아직 제공하지 않는 기능을 사용 가능한 것처럼 쓰지 않습니다.
- 공개 API와 운영 계약을 변경하면 관련 문서, fixture와 검증 자료를 같은 PR에서 갱신합니다.

## 안전한 개발 환경

TaskCage는 Linux cgroup v2와 프로세스 종료를 다룹니다. 개발·테스트 시에는 개인 개발 환경 또는 전용 테스트 VM/컨테이너에서만 제한·정리 테스트를 실행하고, 공유 또는 운영 시스템의 cgroup을 대상으로 하지 않습니다.
