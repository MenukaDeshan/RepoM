# Contributing to RepoM

Thank you for helping improve RepoM! Every contribution—bug reports, documentation fixes, tests, and new features—is appreciated.

## How to contribute

1. **Check existing issues and PRs** before starting major work so effort is not duplicated. For new features or large refactors, open a short issue first to discuss the design.
2. **Use one focused change per pull request** on a descriptive branch:
   - `feature/<short-name>`
   - `bugfix/<short-name>`
   - `docs/<short-name>`
   - `test/<short-name>`
   - `refactor/<short-name>`
   - `chore/<short-name>`
3. **Write a clear PR title.** Pull requests are squash-merged, and GitHub automatically generates release notes from merged PR titles (for example: `Fix clipped status bar spinner on dark theme`). Write titles in plain English describing what changed for users or developers.

## Engineering rules

- **Safe Rust only:** Avoid `unsafe` unless strictly necessary and well-justified.
- **Read-only by default:** Scanning and inspecting repositories must never mutate working trees, branches, or Git history. Mutating actions (`pull --ff-only`, `clone`) must require explicit user confirmation and strict safety checks.
- **No secrets or personal data:** Never commit tokens, credentials, local machine paths, or personal repository names in code, fixtures, or tests. Always sanitize URLs before displaying or exporting them.
- **Cross-platform mindset:** Keep Windows, macOS, and Linux support working cleanly (`#[cfg(target_os = ...)]` where platform APIs differ).
- **Tests for behavior changes:** Add unit or integration tests in `src/` or `tests/` whenever you fix a bug or add logic. Tests must be hermetic and never depend on real GitHub credentials or the contributor's local folders.

## Before opening a pull request

Run the full local check suite:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

## Licensing

RepoM is licensed under the [MIT License](LICENSE). By submitting a contribution, you agree that your work will be licensed under the same MIT License.
