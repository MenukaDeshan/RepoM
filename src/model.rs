use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Represents one aspect of a repository's synchronization state.
/// A repository can hold several statuses simultaneously (e.g. `DIRTY` + `AHEAD`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Status {
    Dirty,
    Untracked,
    Ahead,
    Behind,
    Diverged,
    NoUpstream,
    NoRemote,
    NotOnGithub,
    NotCloned,
    Stashed,
    Clean,
}

impl Status {
    /// Display string matching the CLI table and documentation.
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Dirty => "DIRTY",
            Status::Untracked => "UNTRACKED",
            Status::Ahead => "AHEAD",
            Status::Behind => "BEHIND",
            Status::Diverged => "DIVERGED",
            Status::NoUpstream => "NO_UPSTREAM",
            Status::NoRemote => "NO_REMOTE",
            Status::NotOnGithub => "NOT_ON_GITHUB",
            Status::NotCloned => "NOT_CLONED",
            Status::Stashed => "STASHED",
            Status::Clean => "CLEAN",
        }
    }

    /// Lower rank = more urgent / appears first when sorting worst-first.
    pub fn severity_rank(&self) -> u8 {
        match self {
            Status::Diverged => 0,
            Status::Dirty => 1,
            Status::Ahead => 2,
            Status::Untracked => 3,
            Status::Stashed => 4,
            Status::NoRemote => 5,
            Status::NoUpstream => 6,
            Status::NotOnGithub => 7,
            Status::Behind => 8,
            Status::NotCloned => 9,
            Status::Clean => 10,
        }
    }

    /// True if this status indicates the repository is out of sync.
    pub fn is_out_of_sync(&self) -> bool {
        !matches!(self, Status::Clean)
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Status {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let normalized = s.trim().to_ascii_lowercase().replace('_', "-");
        match normalized.as_str() {
            "dirty" => Ok(Status::Dirty),
            "untracked" => Ok(Status::Untracked),
            "ahead" => Ok(Status::Ahead),
            "behind" => Ok(Status::Behind),
            "diverged" => Ok(Status::Diverged),
            "no-upstream" | "noupstream" => Ok(Status::NoUpstream),
            "no-remote" | "noremote" => Ok(Status::NoRemote),
            "not-on-github" | "notongithub" => Ok(Status::NotOnGithub),
            "not-cloned" | "notcloned" => Ok(Status::NotCloned),
            "stashed" | "stash" => Ok(Status::Stashed),
            "clean" => Ok(Status::Clean),
            other => Err(format!(
                "unknown status '{}'. Valid values: dirty, untracked, ahead, behind, diverged, \
                 no-upstream, no-remote, not-on-github, not-cloned, stashed, clean",
                other
            )),
        }
    }
}

/// Information about a single configured git remote (with credentials stripped from the URL).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteInfo {
    pub name: String,
    pub url: String,
}

/// Full synchronization report for a single repository (local, remote-only, or matched).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoReport {
    pub name: String,
    pub path: Option<PathBuf>,
    pub remote_url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remotes: Vec<RemoteInfo>,
    pub branch: Option<String>,
    #[serde(default)]
    pub detached_head: bool,
    #[serde(default)]
    pub is_bare: bool,
    #[serde(default)]
    pub is_worktree: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub submodules: Vec<String>,
    pub ahead: Option<usize>,
    pub behind: Option<usize>,
    pub dirty_files: usize,
    pub untracked_files: usize,
    pub stashes: usize,
    pub statuses: Vec<Status>,
    pub last_commit: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_full_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clone_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_url: Option<String>,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub fork: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl RepoReport {
    /// Returns the worst (lowest-numbered) severity rank among this repo's statuses.
    pub fn worst_severity(&self) -> u8 {
        self.statuses
            .iter()
            .map(Status::severity_rank)
            .min()
            .unwrap_or(Status::Clean.severity_rank())
    }

    /// True if the repo is completely synced (only `CLEAN` and no inspection error).
    pub fn is_clean(&self) -> bool {
        self.error.is_none() && self.statuses.len() == 1 && self.statuses[0] == Status::Clean
    }

    /// True if this repo has the specified status.
    pub fn has_status(&self, status: Status) -> bool {
        self.statuses.contains(&status)
    }

    /// True if the repo is eligible for a safe fast-forward pull (`pull --clean-only`).
    pub fn is_clean_pullable(&self) -> bool {
        self.path.is_some()
            && !self.is_bare
            && !self.detached_head
            && self.error.is_none()
            && self.has_status(Status::Behind)
            && !self.has_status(Status::Dirty)
            && !self.has_status(Status::Untracked)
            && !self.has_status(Status::Diverged)
            && !self.has_status(Status::Ahead)
            && !self.has_status(Status::NoUpstream)
            && !self.has_status(Status::NoRemote)
            && !self.has_status(Status::NotCloned)
    }

    /// Normalize the `statuses` list: deduplicate, remove `Clean` if any non-clean
    /// status is present, ensure `Clean` if empty, and sort by severity.
    pub fn normalize_statuses(&mut self) {
        let mut unique: Vec<Status> = Vec::new();
        for &st in &self.statuses {
            if !unique.contains(&st) {
                unique.push(st);
            }
        }
        if unique.iter().any(|s| s.is_out_of_sync()) {
            unique.retain(|s| s.is_out_of_sync());
        } else if unique.is_empty() {
            unique.push(Status::Clean);
        }
        unique.sort_by_key(Status::severity_rank);
        self.statuses = unique;
    }
}

/// Aggregated counts across all reported repositories.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Summary {
    pub total: usize,
    pub clean: usize,
    pub dirty: usize,
    pub untracked: usize,
    pub ahead: usize,
    pub behind: usize,
    pub diverged: usize,
    pub stashed: usize,
    pub no_upstream: usize,
    pub no_remote: usize,
    pub not_on_github: usize,
    pub not_cloned: usize,
    pub errors: usize,
}

impl Summary {
    pub fn from_reports(reports: &[RepoReport]) -> Self {
        let mut s = Summary {
            total: reports.len(),
            ..Default::default()
        };
        for r in reports {
            if r.error.is_some() {
                s.errors += 1;
            }
            if r.is_clean() {
                s.clean += 1;
                continue;
            }
            if r.has_status(Status::Dirty) {
                s.dirty += 1;
            }
            if r.has_status(Status::Untracked) {
                s.untracked += 1;
            }
            if r.has_status(Status::Ahead) {
                s.ahead += 1;
            }
            if r.has_status(Status::Behind) {
                s.behind += 1;
            }
            if r.has_status(Status::Diverged) {
                s.diverged += 1;
            }
            if r.has_status(Status::Stashed) {
                s.stashed += 1;
            }
            if r.has_status(Status::NoUpstream) {
                s.no_upstream += 1;
            }
            if r.has_status(Status::NoRemote) {
                s.no_remote += 1;
            }
            if r.has_status(Status::NotOnGithub) {
                s.not_on_github += 1;
            }
            if r.has_status(Status::NotCloned) {
                s.not_cloned += 1;
            }
        }
        s
    }

    pub fn has_issues(&self) -> bool {
        self.clean < self.total || self.errors > 0
    }
}

/// Top-level JSON output schema (ready for second-brain ingestion and multi-machine diffs).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanOutput {
    pub machine: String,
    pub root: String,
    pub generated_at: DateTime<Utc>,
    pub summary: Summary,
    pub repos: Vec<RepoReport>,
}

/// Sort reports worst-first, then alphabetically (case-insensitive) by name.
pub fn sort_reports(reports: &mut [RepoReport]) {
    reports.sort_by(|a, b| {
        let err_a = if a.error.is_some() { 0 } else { 1 };
        let err_b = if b.error.is_some() { 0 } else { 1 };
        err_a
            .cmp(&err_b)
            .then_with(|| a.worst_severity().cmp(&b.worst_severity()))
            .then_with(|| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_status_variants() {
        assert_eq!("dirty".parse::<Status>().unwrap(), Status::Dirty);
        assert_eq!("NO_UPSTREAM".parse::<Status>().unwrap(), Status::NoUpstream);
        assert_eq!("no-upstream".parse::<Status>().unwrap(), Status::NoUpstream);
        assert_eq!("not-cloned".parse::<Status>().unwrap(), Status::NotCloned);
        assert_eq!(
            "not_on_github".parse::<Status>().unwrap(),
            Status::NotOnGithub
        );
        assert!("invalid".parse::<Status>().is_err());
    }

    #[test]
    fn normalize_statuses_removes_clean_when_dirty() {
        let mut report = RepoReport {
            name: "demo".into(),
            path: Some(PathBuf::from("/tmp/demo")),
            remote_url: None,
            remotes: vec![],
            branch: Some("main".into()),
            detached_head: false,
            is_bare: false,
            is_worktree: false,
            submodules: vec![],
            ahead: Some(1),
            behind: Some(0),
            dirty_files: 2,
            untracked_files: 0,
            stashes: 0,
            statuses: vec![Status::Clean, Status::Ahead, Status::Dirty, Status::Dirty],
            last_commit: None,
            github_full_name: None,
            clone_url: None,
            ssh_url: None,
            archived: false,
            fork: false,
            error: None,
        };
        report.normalize_statuses();
        assert_eq!(report.statuses, vec![Status::Dirty, Status::Ahead]);
        assert!(!report.is_clean());
    }
}
