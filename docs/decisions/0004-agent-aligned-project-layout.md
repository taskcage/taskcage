# Decision 0004: Align the project layout with AGENTS.md

- Status: Accepted
- Date: 2026-07-20
- Authority: `AGENTS.md`

## Problem

The initial scaffold used implementation-oriented nested paths such as
`crates/taskcaged/` and `sdk/java/`. The repository guidance defines stable,
top-level locations so contributors and coding agents can find the daemon,
SDK, integration tests, fixtures and decisions without interpreting the build
system first.

## Options considered

1. Keep the existing nested paths and treat `AGENTS.md` as illustrative.
2. Add symlinks or duplicate wrapper directories for the documented paths.
3. Move each component to the documented top-level path and update all build,
   CI and documentation references.

## Decision

Use option 3 with the following mapping:

| Previous path | Current path |
|---|---|
| `crates/taskcaged/` | `daemon/` |
| `sdk/java/` | `java-sdk/` |
| `tests/integration/` | `integration-tests/` |
| `crates/taskcage-fixtures/` | `test-fixtures/` |
| `docs/adr/` | `docs/decisions/` |

The root Cargo workspace, Gradle CI command and documentation index must point
only to the current paths. The old directories are removed rather than kept as
aliases.

## Reasons

- The repository instructions explicitly identify these locations.
- Top-level language and test boundaries are visible without tooling.
- Rust and Java contributors can work in independent roots.
- Decision records now match `CONTRIBUTING.md` and `AGENTS.md` exactly.
- Avoiding symlinks prevents platform and packaging ambiguity.

## Verification

- Check that Cargo workspace members are `daemon` and `test-fixtures`.
- Run Rust format, Clippy and workspace tests on Linux when Cargo is available.
- Compile Java 21 sources and run the Gradle test task from `java-sdk`.
- Parse the CI workflow and verify its Java working path.
- Search tracked files for every previous path and require zero active matches.
- Confirm Git recognizes the changes as moves where content is unchanged.

## Remaining risks

- Open links or unmerged branches may still reference previous paths.
- Git hosting may display some small files as delete/add rather than rename.
- The current branch depends on the Rust-only baseline branch until that work
  is merged into `main`.

These risks are handled by documenting the mapping, keeping the move in a
dedicated refactor commit and rebasing or retargeting the PR after the baseline
is merged.

## Related work

- Issue: to be linked when the project-layout issue is created
- Pull request: to be linked when this branch is published
