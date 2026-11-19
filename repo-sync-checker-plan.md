# RepoM : Rust Repo Manager Plan

A Rust CLI that scans your `Work/` folder and your GitHub account, then tells you which repos are **not in sync** (uncommitted changes, unpushed commits, behind remote, missing locally, or not on GitHub at all). Built for 50+ repos across 3 machines.

---

## 1. Goals

- Answer one question fast: **"Is everything I have safely on GitHub?"**
- Run on all  computers with the same config.
- Work offline for local checks, and use the GitHub API only for the remote comparison.
- Never modify anything by default (read-only), with optional safe actions later.
- can view branch name, ahead, behind, dirty, untracked, stashed, last commit, remote url

## 2. What "not in sync" means

| Status | Meaning | Check |
|---|---|---|
| `DIRTY` | Uncommitted changes (modified or staged files) | working tree status |
| `UNTRACKED` | New files never added to git | status, untracked entries |
| `AHEAD` | Local commits not pushed | ahead count vs upstream |
| `BEHIND` | Remote has commits you haven't pulled | behind count vs upstream |
| `DIVERGED` | Both ahead and behind | ahead > 0 and behind > 0 |
| `NO_UPSTREAM` | Branch has no tracking branch | upstream missing |
| `NO_REMOTE` | Local repo has no remote at all | remotes list empty |
| `NOT_ON_GITHUB` | Remote points elsewhere or repo is missing on your account | compare with API list |
| `NOT_CLONED` | Repo on GitHub but missing on this machine | API list minus local list |
| `STASHED` | Has stashes that exist only locally | stash reflog |
| `CLEAN` | Everything synced | all checks pass |

Note: `AHEAD`/`BEHIND` are only accurate after a `git fetch`, so the tool offers a `--fetch` flag.

## 3. Tech stack

| Need | Crate |
|---|---|
| CLI args | `clap` (derive) |
| Local git inspection | `git2` (libgit2 bindings), with a fallback to shelling out to `git` |
| GitHub API | `octocrab` (or `reqwest` + `serde`) |
| Async + parallelism | `tokio`, plus `rayon` for local scans |
| Config | `serde` + `toml` |
| Folder walking | `walkdir` or `ignore` |
| Table output | `comfy-table` |
| Colors | `owo-colors` or `colored` |
| Progress bars | `indicatif` |
| Errors | `anyhow` + `thiserror` |
| JSON output | `serde_json` |

Tip: `git2` can be awkward with SSH auth. For `fetch`, shelling out to `git fetch` is simpler and reuses your existing credentials.

## 4. Config file (`reposync.toml`)

Location: `~/.config/reposync/config.toml` (Windows: `%APPDATA%\reposync\config.toml`)

```toml
root = "C:/Work"                 # D:/Work on the PC
github_user = "your-username"
github_orgs = ["veltrion"]       # optional
token_env = "GITHUB_TOKEN"       # never store the token in the file

[scan]
max_depth = 3
exclude = ["node_modules", "99_Archive", ".next", "target"]

[machine]
name = "pc"                      # pc | office | personal
skip_folders = ["02_Rainco"]     # do not scan or report these on this machine
```

Security: the token is read from an environment variable (or the OS keyring via the `keyring` crate), never from the file or the repo.

## 5. Architecture

```
reposync/
├── Cargo.toml
├── src/
│   ├── main.rs            # entry, clap setup
│   ├── cli.rs             # commands and flags
│   ├── config.rs          # load/validate TOML
│   ├── scanner.rs         # find local repos (walk for .git)
│   ├── local.rs           # git2: dirty, ahead/behind, stash, remotes
│   ├── github.rs          # list remote repos via API (paginated)
│   ├── compare.rs         # merge local + remote into RepoReport
│   ├── model.rs           # RepoReport, Status enum
│   ├── report.rs          # table / json / markdown output
│   └── actions.rs         # optional: fetch, pull, push (phase 4)
└── tests/
```

### Core data model

```rust
struct RepoReport {
    name: String,
    path: Option<PathBuf>,       // None when NOT_CLONED
    remote_url: Option<String>,
    branch: Option<String>,
    ahead: usize,
    behind: usize,
    dirty_files: usize,
    untracked_files: usize,
    stashes: usize,
    statuses: Vec<Status>,       // can hold several at once
    last_commit: Option<DateTime<Utc>>,
}
```

## 6. Commands

```
reposync scan                 # local repos only, offline
reposync check                # local + GitHub comparison
reposync check --fetch        # fetch first for accurate ahead/behind
reposync check --only dirty   # filter by status
reposync check --json         # machine-readable (feeds second brain)
reposync check --md report.md # write a markdown report
reposync missing              # on GitHub but not cloned here
reposync orphans              # local repos with no GitHub match
```

Example output:

```
REPO           BRANCH   STATUS               AHEAD  BEHIND  DIRTY
ratejet        main     DIRTY, AHEAD         2      0       4
uppay          main     CLEAN                0      0       0
pos-saas       dev      NO_UPSTREAM          -      -       0
sentinelpc     main     BEHIND               0      3       0
clobbit-site   -        NOT_CLONED           -      -       -

Summary: 52 repos | 41 clean | 6 dirty | 3 ahead | 2 behind | 1 not cloned
```

Exit codes: `0` all clean, `1` something out of sync (useful for scripts and scheduled tasks).

## 7. Algorithm

1. Load config and skip folders for this machine.
2. **Scan local:** walk `root` to `max_depth`, collect every folder containing `.git`, and do not descend into repos.
3. **Inspect each repo in parallel** (rayon):
   - open with `git2::Repository::open`
   - `statuses()` for modified, staged and untracked counts
   - current branch, upstream, and `graph_ahead_behind(local, upstream)`
   - remotes, stash count (`stash_foreach`)
4. **Fetch GitHub list:** paginate `/user/repos` (and `/orgs/{org}/repos`), 100 per page, and build a map `full_name -> clone_url`.
5. **Match local to remote** by normalizing remote URLs (SSH and HTTPS forms to `owner/repo`).
6. **Compare:**
   - local repo with no match: `NOT_ON_GITHUB`
   - GitHub repo with no local match: `NOT_CLONED`
7. Derive statuses, sort (worst first), and render.

URL normalization is the part most likely to cause bugs. Handle `git@github.com:owner/repo.git`, `https://github.com/owner/repo.git`, and the no-`.git` variants.

## 8. Phased roadmap

### Phase 1: Local scanner (weekend)
- [ ] `cargo new reposync`, add clap, git2, walkdir, comfy-table
- [ ] Config loader
- [ ] Find repos, then report dirty, ahead, behind, no upstream
- [ ] Table output

**Done when:** `reposync scan` correctly flags every local repo's state.

### Phase 2: GitHub comparison
- [ ] Token via env var, paginated repo listing
- [ ] URL normalization and matching
- [ ] `NOT_CLONED` and `NOT_ON_GITHUB` statuses
- [ ] `missing` and `orphans` commands

**Done when:** the repo counts on GitHub and locally reconcile exactly.

### Phase 3: Accuracy and output
- [ ] `--fetch` (parallel, with a timeout per repo)
- [ ] Stash detection, last commit date
- [ ] JSON and markdown report output
- [ ] Filters and exit codes

### Phase 4: Safe actions (optional)
- [ ] `reposync pull --clean-only` (fast-forward only, skips dirty repos)
- [ ] `reposync clone-missing` (clones everything `NOT_CLONED`)
- [ ] `--dry-run` as default, and `--yes` to apply
- [ ] Never force-push, never reset, never delete

### Phase 5: Integrations
- [ ] Write the JSON report into `05_Second-Brain/` so Antigravity uploads it
- [ ] Scheduled run (Windows Task Scheduler) at login or end of day, plus a desktop notification when something is unpushed
- [ ] Optional TUI with `ratatui` (interactive list, press a key to open a repo)
- [ ] Per-machine report so you can compare state across PC, office, and personal laptops

## 9. Edge cases to handle

- Detached HEAD, with no branch name
- Repos with several remotes (`origin` and `upstream`)
- Submodules (report, but do not recurse at first)
- Bare repos and worktrees
- Very large repos (use libgit2 status options to skip ignored files)
- GitHub rate limits (authenticated is 5000/hour, which is plenty, but handle 403/429)
- Renamed or transferred repos (GitHub redirects, so URL matching can miss them)
- Archived and forked repos (optional filter flags)
- Windows long paths and `\` vs `/` normalization
- Repos that exist on GitHub under a different org than configured

## 10. Testing

- Unit tests: URL normalization, status derivation
- Integration tests: build temporary repos in a `tempdir` (dirty, ahead, behind, no remote) and assert the report
- Mock the GitHub API with `wiremock` or `httpmock`
- Manual test on a copy of your real `Work/` folder before trusting Phase 4 actions

## 11. Security notes

- Read-only by default
- Token comes from an environment variable or keyring, and is never logged or written to reports
- Use a fine-grained token with **read-only** repository metadata for Phase 1 to 3
- Skip `02_Rainco` on personal machines through `skip_folders`, so company repo names never appear in reports synced to your second brain

## 12. Distribution

- `cargo build --release` gives a single `.exe`, so copy it to all 3 machines
- Later: GitHub Actions to build release binaries, and `cargo install --path .` for quick updates
- Keep the project in `01_Veltrion/` or `04_Personal/` as its own repo

## 13. Possible names

`reposync`, `gitgauge`, `repo-radar`, `syncwatch`, `pushcheck`

---

**Suggested first step:** build Phase 1 only, run it against your real `Work/` folder, and see how many repos are surprising you. That alone will probably justify the rest.
