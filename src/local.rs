use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use chrono::{DateTime, TimeZone, Utc};
use git2::{BranchType, ErrorCode, Repository, Status as GitStatus, StatusOptions};
use rayon::prelude::*;

use crate::compare::sanitize_remote_url;
use crate::model::{sort_reports, RemoteInfo, RepoReport, Status};

/// Inspect a list of local repository paths in parallel using `rayon`.
pub fn inspect_repos_parallel(paths: &[PathBuf]) -> Vec<RepoReport> {
    inspect_repos_with_progress(paths, |_| {})
}

pub(crate) fn inspect_repos_with_progress(
    paths: &[PathBuf],
    on_report: impl Fn(&RepoReport) + Sync,
) -> Vec<RepoReport> {
    let mut reports: Vec<RepoReport> = paths
        .par_iter()
        .map(|path| {
            let report = inspect_repo(path);
            on_report(&report);
            report
        })
        .collect();
    sort_reports(&mut reports);
    reports
}

/// Inspect a single local git repository.
/// Uses `git2` as the primary engine and falls back to shelling out to `git` if `git2` cannot open the repo.
pub fn inspect_repo(path: &Path) -> RepoReport {
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());

    match inspect_with_git2(path, &name) {
        Ok(report) => report,
        Err(git2_err) => match inspect_with_git_cli(path, &name) {
            Ok(report) => report,
            Err(_) => RepoReport {
                name,
                path: Some(path.to_path_buf()),
                remote_url: None,
                remotes: Vec::new(),
                branch: None,
                detached_head: false,
                is_bare: false,
                is_worktree: false,
                submodules: Vec::new(),
                ahead: None,
                behind: None,
                dirty_files: 0,
                untracked_files: 0,
                stashes: 0,
                statuses: Vec::new(),
                last_commit: None,
                github_full_name: None,
                clone_url: None,
                ssh_url: None,
                archived: false,
                fork: false,
                error: Some(format!("failed to inspect repo: {}", git2_err)),
            },
        },
    }
}

fn inspect_with_git2(path: &Path, name: &str) -> Result<RepoReport> {
    let mut repo = Repository::open(path)
        .with_context(|| format!("git2 could not open {}", path.display()))?;

    let is_bare = repo.is_bare();
    let is_worktree = repo.is_worktree();

    // Remotes (sanitized so credentials never appear in output)
    let remotes = collect_remotes_git2(&repo);
    let primary_remote = remotes
        .iter()
        .find(|r| r.name == "origin")
        .or_else(|| remotes.first())
        .map(|r| r.url.clone());

    // Submodules (report names without recursing)
    let submodules: Vec<String> = repo
        .submodules()
        .map(|subs| {
            subs.into_iter()
                .filter_map(|s| s.name().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // HEAD, branch, detached state, last commit, and upstream ahead/behind
    let mut branch_name: Option<String> = None;
    let mut detached_head = false;
    let mut last_commit: Option<DateTime<Utc>> = None;
    let mut ahead: Option<usize> = None;
    let mut behind: Option<usize> = None;
    let mut has_upstream = false;
    let mut unborn_head = false;

    match repo.head() {
        Ok(head) => {
            detached_head = repo.head_detached().unwrap_or(false);
            if let Ok(commit) = head.peel_to_commit() {
                let secs = commit.time().seconds();
                last_commit = Utc.timestamp_opt(secs, 0).single();
            }

            if detached_head {
                if let Some(oid) = head.target() {
                    let short = oid.to_string();
                    branch_name = Some(format!("detached@{}", &short[..short.len().min(7)]));
                }
            } else if head.is_branch() {
                let shorthand = head.shorthand().map(String::from);
                if let Some(ref bname) = shorthand {
                    if let Ok(local_branch) = repo.find_branch(bname, BranchType::Local) {
                        if let Ok(upstream) = local_branch.upstream() {
                            has_upstream = true;
                            if let (Some(local_oid), Some(upstream_oid)) =
                                (local_branch.get().target(), upstream.get().target())
                            {
                                if let Ok((a, b)) = repo.graph_ahead_behind(local_oid, upstream_oid)
                                {
                                    ahead = Some(a);
                                    behind = Some(b);
                                }
                            }
                        }
                    }
                }
                branch_name = shorthand;
            }
        }
        Err(e) if e.code() == ErrorCode::UnbornBranch => {
            unborn_head = true;
            // Try reading HEAD symbolic ref to get the unborn branch name (e.g. `main`)
            if let Ok(head_ref) = repo.find_reference("HEAD") {
                if let Some(sym) = head_ref.symbolic_target() {
                    branch_name = Some(
                        sym.strip_prefix("refs/heads/")
                            .unwrap_or(sym)
                            .to_string(),
                    );
                }
            }
        }
        Err(_) => {}
    }

    // Working tree status (skip for bare repos; skip ignored files for performance on large repos)
    let mut dirty_files = 0usize;
    let mut untracked_files = 0usize;

    if !is_bare {
        let mut opts = StatusOptions::new();
        opts.include_untracked(true)
            .recurse_untracked_dirs(false)
            .include_ignored(false)
            .include_unmodified(false)
            .exclude_submodules(false);

        if let Ok(statuses) = repo.statuses(Some(&mut opts)) {
            let dirty_mask = GitStatus::INDEX_NEW
                | GitStatus::INDEX_MODIFIED
                | GitStatus::INDEX_DELETED
                | GitStatus::INDEX_RENAMED
                | GitStatus::INDEX_TYPECHANGE
                | GitStatus::WT_MODIFIED
                | GitStatus::WT_DELETED
                | GitStatus::WT_TYPECHANGE
                | GitStatus::WT_RENAMED
                | GitStatus::CONFLICTED;

            for entry in statuses.iter() {
                let st = entry.status();
                if st.intersects(dirty_mask) {
                    dirty_files += 1;
                }
                if st.contains(GitStatus::WT_NEW) {
                    untracked_files += 1;
                }
            }
        }
    }

    // Stash count (skip for bare repos)
    let mut stashes = 0usize;
    if !is_bare {
        let _ = repo.stash_foreach(|_idx, _msg, _oid| {
            stashes += 1;
            true
        });
    }

    let statuses = derive_local_statuses(
        dirty_files,
        untracked_files,
        ahead,
        behind,
        stashes,
        remotes.is_empty(),
        has_upstream,
        detached_head,
        is_bare,
        unborn_head,
    );

    let mut report = RepoReport {
        name: name.to_string(),
        path: Some(path.to_path_buf()),
        remote_url: primary_remote,
        remotes,
        branch: branch_name,
        detached_head,
        is_bare,
        is_worktree,
        submodules,
        ahead,
        behind,
        dirty_files,
        untracked_files,
        stashes,
        statuses,
        last_commit,
        github_full_name: None,
        clone_url: None,
        ssh_url: None,
        archived: false,
        fork: false,
        error: None,
    };
    report.normalize_statuses();
    Ok(report)
}

fn collect_remotes_git2(repo: &Repository) -> Vec<RemoteInfo> {
    let mut list = Vec::new();
    if let Ok(names) = repo.remotes() {
        for name in names.iter().flatten() {
            if let Ok(remote) = repo.find_remote(name) {
                if let Some(url) = remote.url() {
                    list.push(RemoteInfo {
                        name: name.to_string(),
                        url: sanitize_remote_url(url),
                    });
                }
            }
        }
    }
    list
}

#[allow(clippy::too_many_arguments)]
pub fn derive_local_statuses(
    dirty_files: usize,
    untracked_files: usize,
    ahead: Option<usize>,
    behind: Option<usize>,
    stashes: usize,
    no_remotes: bool,
    has_upstream: bool,
    detached_head: bool,
    is_bare: bool,
    unborn_head: bool,
) -> Vec<Status> {
    let mut statuses = Vec::new();

    if dirty_files > 0 {
        statuses.push(Status::Dirty);
    }
    if untracked_files > 0 {
        statuses.push(Status::Untracked);
    }

    let a = ahead.unwrap_or(0);
    let b = behind.unwrap_or(0);
    if a > 0 && b > 0 {
        statuses.push(Status::Diverged);
    } else if a > 0 {
        statuses.push(Status::Ahead);
    } else if b > 0 {
        statuses.push(Status::Behind);
    }

    if stashes > 0 {
        statuses.push(Status::Stashed);
    }

    if no_remotes {
        statuses.push(Status::NoRemote);
    } else if !has_upstream && !detached_head && !is_bare {
        statuses.push(Status::NoUpstream);
    }
    let _ = unborn_head;

    if statuses.is_empty() {
        statuses.push(Status::Clean);
    }

    statuses
}

/// Fallback repository inspection using the `git` CLI when `git2` cannot open a repo
/// (e.g., newer git repository extensions such as reftable or sha256 object format).
fn inspect_with_git_cli(path: &Path, name: &str) -> Result<RepoReport> {
    let run_git = |args: &[&str]| -> Result<String> {
        let mut cmd = Command::new("git");
        cmd.args(args)
            .current_dir(path)
            .env("GIT_TERMINAL_PROMPT", "0");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        let out = cmd
            .output()
            .with_context(|| format!("failed to invoke git in {}", path.display()))?;
        if !out.status.success() {
            anyhow::bail!("git {:?} exited with {}", args, out.status);
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    };

    let is_bare = run_git(&["rev-parse", "--is-bare-repository"])
        .map(|s| s.trim() == "true")
        .unwrap_or(false);

    // Remotes
    let mut remotes = Vec::new();
    if let Ok(remote_names) = run_git(&["remote"]) {
        for rname in remote_names.lines().map(str::trim).filter(|s| !s.is_empty()) {
            if let Ok(url) = run_git(&["remote", "get-url", rname]) {
                remotes.push(RemoteInfo {
                    name: rname.to_string(),
                    url: sanitize_remote_url(url.trim()),
                });
            }
        }
    }
    let primary_remote = remotes
        .iter()
        .find(|r| r.name == "origin")
        .or_else(|| remotes.first())
        .map(|r| r.url.clone());

    // Branch & detached HEAD
    let branch_raw = run_git(&["symbolic-ref", "--short", "HEAD"]).ok();
    let detached_head = branch_raw.is_none() && run_git(&["rev-parse", "HEAD"]).is_ok();
    let branch = match branch_raw {
        Some(b) => Some(b.trim().to_string()),
        None if detached_head => run_git(&["rev-parse", "--short", "HEAD"])
            .ok()
            .map(|sha| format!("detached@{}", sha.trim())),
        None => None,
    };

    // Last commit timestamp
    let last_commit = run_git(&["log", "-1", "--format=%ct"])
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .and_then(|secs| Utc.timestamp_opt(secs, 0).single());
    let unborn_head = last_commit.is_none();

    // Upstream & ahead/behind
    let mut has_upstream = false;
    let mut ahead = None;
    let mut behind = None;
    if !detached_head && !is_bare {
        if let Ok(counts) = run_git(&["rev-list", "--left-right", "--count", "HEAD...@{u}"]) {
            let mut parts = counts.split_whitespace();
            if let (Some(a), Some(b)) = (parts.next(), parts.next()) {
                if let (Ok(a_num), Ok(b_num)) = (a.parse::<usize>(), b.parse::<usize>()) {
                    has_upstream = true;
                    ahead = Some(a_num);
                    behind = Some(b_num);
                }
            }
        }
    }

    // Dirty & untracked
    let mut dirty_files = 0usize;
    let mut untracked_files = 0usize;
    if !is_bare {
        if let Ok(porcelain) = run_git(&["status", "--porcelain=v1", "--untracked-files=normal"]) {
            for line in porcelain.lines() {
                if line.starts_with("??") {
                    untracked_files += 1;
                } else if !line.trim().is_empty() {
                    dirty_files += 1;
                }
            }
        }
    }

    // Stashes
    let stashes = if !is_bare {
        run_git(&["stash", "list"])
            .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
            .unwrap_or(0)
    } else {
        0
    };

    let statuses = derive_local_statuses(
        dirty_files,
        untracked_files,
        ahead,
        behind,
        stashes,
        remotes.is_empty(),
        has_upstream,
        detached_head,
        is_bare,
        unborn_head,
    );

    let mut report = RepoReport {
        name: name.to_string(),
        path: Some(path.to_path_buf()),
        remote_url: primary_remote,
        remotes,
        branch,
        detached_head,
        is_bare,
        is_worktree: false,
        submodules: Vec::new(),
        ahead,
        behind,
        dirty_files,
        untracked_files,
        stashes,
        statuses,
        last_commit,
        github_full_name: None,
        clone_url: None,
        ssh_url: None,
        archived: false,
        fork: false,
        error: None,
    };
    report.normalize_statuses();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_diverged_when_both_ahead_and_behind() {
        let st = derive_local_statuses(0, 0, Some(2), Some(3), 0, false, true, false, false, false);
        assert_eq!(st, vec![Status::Diverged]);
    }

    #[test]
    fn derives_clean_when_all_zero_and_has_upstream() {
        let st = derive_local_statuses(0, 0, Some(0), Some(0), 0, false, true, false, false, false);
        assert_eq!(st, vec![Status::Clean]);
    }

    #[test]
    fn derives_multiple_statuses_simultaneously() {
        let st = derive_local_statuses(2, 1, Some(3), Some(0), 1, false, true, false, false, false);
        assert!(st.contains(&Status::Dirty));
        assert!(st.contains(&Status::Untracked));
        assert!(st.contains(&Status::Ahead));
        assert!(st.contains(&Status::Stashed));
    }
}
