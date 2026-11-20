# Changelog

All notable changes to RepoM are documented here and on the [GitHub Releases](https://github.com/MenukaDeshan/RepoM/releases) page.

This project follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1] - 2026-11-20

### Fixed
- Show local repositories as their status checks complete, with scan progress and guidance when no repositories are found.
- Fixed the bottom status bar spinner overflowing and pushing status text out of alignment during scans and Git actions.

### Added
- Open-source documentation (`LICENSE`, `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `SECURITY.md`, `CONTRIBUTORS.md`, `CHANGELOG.md`).
- Automated CI checks and multi-platform GitHub Release workflow with auto-generated release notes and SHA-256 checksums.
- Standalone Windows setup installer (`packaging/windows/repom.iss` + `packaging/windows/package.ps1`) with statically linked MSVC C runtime (`+crt-static`) so RepoM runs on fresh Windows PCs without extra runtime packages.

## [0.1.0] - 2026-11-19

### Added
- Native pure-Rust desktop GUI (`eframe` / `egui`) with Dark and Light themes.
- Parallel local Git repository scanner (`git2` + `rayon`) with bounded parallel `git fetch` via `tokio`.
- GitHub user and organization repository comparison (`NOT_ON_GITHUB`, `NOT_CLONED`, archived/fork metadata).
- Clickable KPI summary cards, Sync Health progress bar, instant search, status filters, and sorting.
- Slide-out Repository Inspector with safe `git pull --ff-only` (clean behind repos only), `git clone` (HTTPS/SSH), Explorer / VS Code / Terminal / GitHub quick actions, and JSON / Markdown report export.
- Built-in Settings view and zero-secret TOML configuration (`token_env` validation and credential URL sanitization).

[Unreleased]: https://github.com/MenukaDeshan/RepoM/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/MenukaDeshan/RepoM/releases/tag/v0.1.1
[0.1.0]: https://github.com/MenukaDeshan/RepoM/releases/tag/v0.1.0
