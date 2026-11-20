<!-- Pull requests are squash-merged, and the PR title becomes the entry in
GitHub's auto-generated release notes ("What's Changed"). Write a clear, plain-English
title describing what changed, e.g. "Fix clipped status bar spinner during scans". -->

## What changed

<!-- Brief summary of the change and why it is needed. -->

## Verified

<!-- Tests run and manual checks performed (e.g. Windows/Linux/macOS GUI check, git fetch/pull/clone). -->

## Checklist

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --all-targets -- -D warnings`
- [ ] `cargo test --all-targets`
- [ ] No tokens, credentials, or local machine paths added to code, config, or tests
- [ ] Updated `README.md` or `CHANGELOG.md` if user-facing behavior changed
