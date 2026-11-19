use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tokio::process::Command;
use tokio::sync::Semaphore;
use tokio::time::timeout;
use url::Url;

use crate::model::{RepoReport, Status};
use crate::scanner::normalize_display_path;

const MAX_CONCURRENT_GIT_OPS: usize = 8;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Outcome of a single git action (`fetch`, `pull`, or `clone`).
#[derive(Debug, Clone)]
pub struct ActionOutcome {
    pub repo_name: String,
    pub target: PathBuf,
    pub applied: bool,
    pub success: bool,
    pub message: String,
}

/// Run `git fetch --all --prune` across local repositories in parallel with bounded concurrency
/// and a per-repo timeout. Uses `GIT_TERMINAL_PROMPT=0` so it never hangs waiting for credentials.
pub async fn fetch_repos_parallel(
    repo_paths: &[PathBuf],
    timeout_secs: u64,
) -> Vec<(PathBuf, Result<(), String>)> {
    if repo_paths.is_empty() {
        return Vec::new();
    }

    let sem = Arc::new(Semaphore::new(MAX_CONCURRENT_GIT_OPS));
    let mut handles = Vec::with_capacity(repo_paths.len());

    for path in repo_paths {
        let permit_sem = Arc::clone(&sem);
        let repo_path = path.clone();
        let dur = Duration::from_secs(timeout_secs.max(1));

        handles.push(tokio::spawn(async move {
            let _permit = permit_sem.acquire_owned().await;
            let res = run_git_with_timeout(
                &repo_path,
                &["fetch", "--all", "--prune", "--quiet"],
                dur,
            )
            .await;
            (repo_path, res)
        }));
    }

    let mut results = Vec::with_capacity(handles.len());
    for h in handles {
        if let Ok(item) = h.await {
            results.push(item);
        }
    }

    results
}

/// Phase 4 safe action: fast-forward pull only clean, behind repositories.
/// Never resets, never merges, never touches dirty/untracked/diverged repos.
/// Dry-runs by default unless `apply == true` (`--yes`).
pub async fn pull_clean_repos(
    reports: &[RepoReport],
    apply: bool,
    timeout_secs: u64,
) -> Vec<ActionOutcome> {
    let mut outcomes = Vec::new();
    let dur = Duration::from_secs(timeout_secs.max(1));

    for r in reports {
        if !r.has_status(Status::Behind) && !r.has_status(Status::Diverged) {
            continue;
        }
        let Some(ref path) = r.path else {
            continue;
        };

        if !r.is_clean_pullable() {
            outcomes.push(ActionOutcome {
                repo_name: r.name.clone(),
                target: path.clone(),
                applied: false,
                success: true,
                message: format!(
                    "SKIPPED (not clean: {})",
                    r.statuses
                        .iter()
                        .map(Status::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
            continue;
        }

        if !apply {
            outcomes.push(ActionOutcome {
                repo_name: r.name.clone(),
                target: path.clone(),
                applied: false,
                success: true,
                message: format!(
                    "DRY-RUN: would run `git pull --ff-only` (behind by {})",
                    r.behind.unwrap_or(0)
                ),
            });
            continue;
        }

        match run_git_with_timeout(path, &["pull", "--ff-only"], dur).await {
            Ok(()) => outcomes.push(ActionOutcome {
                repo_name: r.name.clone(),
                target: path.clone(),
                applied: true,
                success: true,
                message: "PULLED (fast-forward)".to_string(),
            }),
            Err(err) => outcomes.push(ActionOutcome {
                repo_name: r.name.clone(),
                target: path.clone(),
                applied: true,
                success: false,
                message: format!("FAILED: {}", err),
            }),
        }
    }

    outcomes
}

/// Phase 4 safe action: clone repositories that exist on GitHub (`NOT_CLONED`) into `dest_dir`.
/// Strictly validates destination boundary inside `root_dir`, repo directory names, and clone URLs.
/// Never overwrites existing directories. Dry-runs by default unless `apply == true` (`--yes`).
pub async fn clone_missing_repos(
    reports: &[RepoReport],
    root_dir: &Path,
    dest_dir: &Path,
    prefer_ssh: bool,
    apply: bool,
    timeout_secs: u64,
) -> Result<Vec<ActionOutcome>> {
    let canonical_root = normalize_display_path(
        &fs::canonicalize(root_dir)
            .with_context(|| format!("failed to canonicalize root {}", root_dir.display()))?,
    );

    if apply && !dest_dir.exists() {
        fs::create_dir_all(dest_dir)
            .with_context(|| format!("failed to create destination {}", dest_dir.display()))?;
    }

    let canonical_dest = if dest_dir.exists() {
        normalize_display_path(
            &fs::canonicalize(dest_dir)
                .with_context(|| format!("failed to canonicalize dest {}", dest_dir.display()))?,
        )
    } else {
        // Validate that the requested destination resolves inside root even during dry-run
        validate_path_within_boundary(&canonical_root, dest_dir)?;
        dest_dir.to_path_buf()
    };

    // Security: ensure destination directory is equal to or strictly inside `root`
    ensure_within_root(&canonical_root, &canonical_dest)?;

    let dur = Duration::from_secs(timeout_secs.max(1));
    let mut outcomes = Vec::new();

    for r in reports {
        if !r.has_status(Status::NotCloned) {
            continue;
        }

        // Strict repo folder name validation (prevent path traversal via repo name)
        if let Err(err) = validate_repo_dir_name(&r.name) {
            outcomes.push(ActionOutcome {
                repo_name: r.name.clone(),
                target: canonical_dest.clone(),
                applied: false,
                success: false,
                message: format!("SKIPPED (unsafe repo name: {})", err),
            });
            continue;
        }

        let target = canonical_dest.join(&r.name);
        if let Err(err) = ensure_within_root(&canonical_dest, &target) {
            outcomes.push(ActionOutcome {
                repo_name: r.name.clone(),
                target,
                applied: false,
                success: false,
                message: format!("SKIPPED (boundary check failed: {})", err),
            });
            continue;
        }

        if target.exists() {
            outcomes.push(ActionOutcome {
                repo_name: r.name.clone(),
                target,
                applied: false,
                success: true,
                message: "SKIPPED (target directory already exists)".to_string(),
            });
            continue;
        }

        let raw_url = if prefer_ssh {
            r.ssh_url.as_deref().or(r.clone_url.as_deref())
        } else {
            r.clone_url.as_deref().or(r.ssh_url.as_deref())
        };

        let Some(url) = raw_url else {
            outcomes.push(ActionOutcome {
                repo_name: r.name.clone(),
                target,
                applied: false,
                success: false,
                message: "SKIPPED (no clone URL available)".to_string(),
            });
            continue;
        };

        if let Err(err) = validate_clone_url(url) {
            outcomes.push(ActionOutcome {
                repo_name: r.name.clone(),
                target,
                applied: false,
                success: false,
                message: format!("SKIPPED (invalid clone URL: {})", err),
            });
            continue;
        }

        if !apply {
            outcomes.push(ActionOutcome {
                repo_name: r.name.clone(),
                target: target.clone(),
                applied: false,
                success: true,
                message: format!("DRY-RUN: would clone {} -> {}", url, target.display()),
            });
            continue;
        }

        let target_str = target.to_string_lossy().to_string();
        // Use `--` before positional args so git cannot interpret a URL or path as a flag.
        match run_git_with_timeout(&canonical_dest, &["clone", "--", url, &target_str], dur).await {
            Ok(()) => outcomes.push(ActionOutcome {
                repo_name: r.name.clone(),
                target,
                applied: true,
                success: true,
                message: format!("CLONED ({})", url),
            }),
            Err(err) => outcomes.push(ActionOutcome {
                repo_name: r.name.clone(),
                target,
                applied: true,
                success: false,
                message: format!("FAILED: {}", err),
            }),
        }
    }

    Ok(outcomes)
}

/// Validate that a repository name is a single safe directory component (no separators, no `..`, no leading `-`).
pub fn validate_repo_dir_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.starts_with('-')
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
        || name.contains(':')
    {
        bail!("'{}' is not a safe directory name", name);
    }
    Ok(())
}

/// Validate a clone URL before passing it to `git clone`.
/// Rejects leading `-` (option injection) and only permits GitHub HTTPS or SSH URLs.
pub fn validate_clone_url(raw: &str) -> Result<()> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.starts_with('-') {
        bail!("URL must not be empty or start with '-'");
    }
    if trimmed.starts_with("git@github.com:") {
        let rest = &trimmed["git@github.com:".len()..];
        if rest.is_empty() || rest.contains("..") || rest.contains(char::is_whitespace) {
            bail!("malformed SSH clone URL");
        }
        return Ok(());
    }
    let parsed = Url::parse(trimmed).context("not a valid URL")?;
    if parsed.scheme() != "https" && parsed.scheme() != "ssh" {
        bail!("unsupported clone URL scheme '{}'", parsed.scheme());
    }
    let host = parsed
        .host_str()
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if host != "github.com" {
        bail!("clone URL host must be github.com (got '{}')", host);
    }
    Ok(())
}

/// Verify that `candidate` is equal to `boundary` or strictly a child path of `boundary`,
/// avoiding partial-prefix bypasses (e.g. `/work-evil` vs `/work`).
fn ensure_within_root(boundary: &Path, candidate: &Path) -> Result<()> {
    if candidate == boundary || candidate.starts_with(boundary) {
        Ok(())
    } else {
        bail!(
            "target path {} escapes allowed root {}",
            candidate.display(),
            boundary.display()
        )
    }
}

fn validate_path_within_boundary(boundary: &Path, candidate: &Path) -> Result<()> {
    // Reject any `..` components when the path does not yet exist on disk
    for comp in candidate.components() {
        if matches!(comp, std::path::Component::ParentDir) {
            bail!("destination path must not contain '..' traversal segments");
        }
    }
    let resolved = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        boundary.join(candidate)
    };
    ensure_within_root(boundary, &resolved)
}

async fn run_git_with_timeout(
    cwd: &Path,
    args: &[&str],
    dur: Duration,
) -> std::result::Result<(), String> {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .kill_on_drop(true);

    #[cfg(windows)]
    {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    match timeout(dur, cmd.output()).await {
        Ok(Ok(output)) if output.status.success() => Ok(()),
        Ok(Ok(output)) => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let detail = if stderr.is_empty() {
                format!("exit status {}", output.status)
            } else {
                stderr.lines().next().unwrap_or("git error").to_string()
            };
            Err(detail)
        }
        Ok(Err(io_err)) => Err(format!("failed to spawn git: {}", io_err)),
        Err(_) => Err(format!("timed out after {}s", dur.as_secs())),
    }
}

/// Safely open a verified local repository folder in Explorer, VS Code, or Terminal.
pub fn open_local_target(path: &Path, target: &str) -> std::result::Result<String, String> {
    if !path.is_dir() {
        return Err("repository directory does not exist on disk".to_string());
    }
    match target {
        "explorer" => {
            #[cfg(target_os = "windows")]
            let res = StdCommand::new("explorer.exe").arg(path).spawn();
            #[cfg(target_os = "macos")]
            let res = StdCommand::new("open").arg(path).spawn();
            #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
            let res = StdCommand::new("xdg-open").arg(path).spawn();

            res.map(|_| format!("Opened {} in File Explorer.", path.display()))
                .map_err(|e| format!("failed to open File Explorer: {}", e))
        }
        "vscode" => {
            #[cfg(target_os = "windows")]
            let res = {
                use std::os::windows::process::CommandExt;
                let mut candidates: Vec<PathBuf> = vec![
                    PathBuf::from("code.cmd"),
                    PathBuf::from("code"),
                ];
                if let Ok(local_app) = std::env::var("LOCALAPPDATA") {
                    candidates.push(
                        PathBuf::from(&local_app)
                            .join("Programs")
                            .join("Microsoft VS Code")
                            .join("bin")
                            .join("code.cmd"),
                    );
                    candidates.push(
                        PathBuf::from(&local_app)
                            .join("Programs")
                            .join("Microsoft VS Code")
                            .join("Code.exe"),
                    );
                }
                candidates.push(PathBuf::from(
                    r"C:\Program Files\Microsoft VS Code\bin\code.cmd",
                ));
                candidates.push(PathBuf::from(
                    r"C:\Program Files\Microsoft VS Code\Code.exe",
                ));

                let mut last_err = None;
                let mut spawned = None;
                for bin in candidates {
                    match StdCommand::new(&bin)
                        .arg(path)
                        .creation_flags(CREATE_NO_WINDOW)
                        .spawn()
                    {
                        Ok(child) => {
                            spawned = Some(child);
                            break;
                        }
                        Err(e) => last_err = Some(e),
                    }
                }
                spawned.ok_or_else(|| {
                    last_err.unwrap_or_else(|| {
                        std::io::Error::new(std::io::ErrorKind::NotFound, "VS Code not found")
                    })
                })
            };
            #[cfg(not(target_os = "windows"))]
            let res = StdCommand::new("code").arg(path).spawn();

            res.map(|_| format!("Opened {} in VS Code.", path.display()))
                .map_err(|e| format!("failed to launch VS Code (`code` not found): {}", e))
        }
        "terminal" => {
            #[cfg(target_os = "windows")]
            let res = StdCommand::new("wt.exe")
                .args(["-d"])
                .arg(path)
                .spawn()
                .or_else(|_| {
                    StdCommand::new("powershell.exe")
                        .arg("-NoExit")
                        .current_dir(path)
                        .spawn()
                });
            #[cfg(target_os = "macos")]
            let res = StdCommand::new("open")
                .args(["-a", "Terminal"])
                .arg(path)
                .spawn();
            #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
            let res = StdCommand::new("x-terminal-emulator")
                .current_dir(path)
                .spawn();

            res.map(|_| format!("Opened terminal in {}.", path.display()))
                .map_err(|e| format!("failed to open terminal: {}", e))
        }
        _ => Err("unsupported open target".to_string()),
    }
}

/// Safely open a validated `https://github.com/...` repository URL in the default browser.
pub fn open_github_url(raw_url: &str) -> std::result::Result<String, String> {
    let parsed = Url::parse(raw_url).map_err(|_| "invalid URL".to_string())?;
    let host = parsed
        .host_str()
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if parsed.scheme() != "https" || host != "github.com" {
        return Err("refusing to open non-GitHub or non-HTTPS URL".to_string());
    }
    let clean = parsed.to_string();

    #[cfg(target_os = "windows")]
    let res = StdCommand::new("rundll32.exe")
        .args(["url.dll,FileProtocolHandler", &clean])
        .spawn();
    #[cfg(target_os = "macos")]
    let res = StdCommand::new("open").arg(&clean).spawn();
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    let res = StdCommand::new("xdg-open").arg(&clean).spawn();

    res.map(|_| format!("Opened {} in browser.", clean))
        .map_err(|e| format!("failed to open browser: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_clone_urls_strictly() {
        assert!(validate_clone_url("https://github.com/alice/demo.git").is_ok());
        assert!(validate_clone_url("git@github.com:alice/demo.git").is_ok());
        assert!(validate_clone_url("ssh://git@github.com/alice/demo.git").is_ok());

        // Reject flag injection & non-github or insecure schemes
        assert!(validate_clone_url("--upload-pack=evil").is_err());
        assert!(validate_clone_url("file:///etc/passwd").is_err());
        assert!(validate_clone_url("http://github.com/alice/demo.git").is_err());
        assert!(validate_clone_url("https://evil.example.com/a/b.git").is_err());
    }

    #[test]
    fn validates_repo_dir_names() {
        assert!(validate_repo_dir_name("my-repo_1.0").is_ok());
        assert!(validate_repo_dir_name("..").is_err());
        assert!(validate_repo_dir_name("../escape").is_err());
        assert!(validate_repo_dir_name("a\\b").is_err());
        assert!(validate_repo_dir_name("-flag").is_err());
    }
}
