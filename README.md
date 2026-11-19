# RepoM (`repom.exe`)

A **100% pure-Rust** native Windows desktop application (single standalone `.exe`, no CLI window, no browser/WebView/Node/JS runtime) that scans your local `Work/` folder and compares it against your GitHub account to answer one question fast: **"Is everything I have safely on GitHub?"**

## Features

- **Native Rust GUI (`eframe` / `egui`)** — double-click `repom.exe` to open the desktop dashboard directly.
- **Fast parallel scanning** — inspects 50+ local repositories across CPU cores using `git2` + `rayon`, plus bounded parallel `git fetch` via `tokio` (without flashing console windows).
- **Full sync visibility** — `DIRTY`, `UNTRACKED`, `AHEAD`, `BEHIND`, `DIVERGED`, `STASHED`, `NO_UPSTREAM`, `NO_REMOTE`, `NOT_ON_GITHUB`, `NOT_CLONED`, and `CLEAN`.
- **Clickable Health Summary Cards & Sync Health Bar** — instant search, status filter chips, sorting, archived/fork toggles, and Dark/Light theme.
- **Repository Inspector & Safe Actions** — Fetch Remote, Pull (`git pull --ff-only` strictly on clean behind repos with confirmation dialog), Clone Missing (HTTPS or SSH with path & URL validation), Open in Explorer / VS Code / Terminal / GitHub, and Export JSON / Markdown reports via native Save dialog.
- **Built-in Settings & Multi-Machine Security** — edit `root`, `github_user`, `github_orgs`, `token_env`, `exclude`, and `skip_folders` right in the app. GitHub tokens are read from `token_env` (default `GITHUB_TOKEN`) or an optional in-memory session field and are **never** written to disk.

## Build & Run

```powershell
# Build a single standalone release executable
cargo build --release

# Launch the desktop app (or double-click target\release\repom.exe in Explorer)
.\target\release\repom.exe
```

Copy `target\release\repom.exe` to all of your Windows machines — zero external dependencies required.
