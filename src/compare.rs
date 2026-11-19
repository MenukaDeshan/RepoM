use std::collections::{HashMap, HashSet};

use url::Url;

use crate::github::GithubRepo;
use crate::model::{sort_reports, RepoReport, Status};

/// Strip embedded credentials (username/password/PAT) from HTTP(S) remote URLs
/// so secrets are never written to stdout, JSON, or Markdown reports.
pub fn sanitize_remote_url(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Ok(mut parsed) = Url::parse(trimmed) {
        if parsed.scheme() == "http" || parsed.scheme() == "https" {
            if !parsed.username().is_empty() || parsed.password().is_some() {
                let _ = parsed.set_username("");
                let _ = parsed.set_password(None);
                return parsed.to_string();
            }
        }
    }
    trimmed.to_string()
}

/// Normalize a GitHub remote URL into lowercase `owner/repo`.
///
/// Supports:
/// - `git@github.com:Owner/Repo.git`
/// - `git@github.com:Owner/Repo`
/// - `ssh://git@github.com/Owner/Repo.git`
/// - `ssh://git@github.com:22/Owner/Repo.git`
/// - `https://github.com/Owner/Repo.git`
/// - `https://github.com/Owner/Repo`
/// - `https://github.com/Owner/Repo/`
/// - `https://user:token@github.com/Owner/Repo.git`
/// - `http://github.com/Owner/Repo.git`
/// - `git://github.com/Owner/Repo.git`
///
/// Returns `None` for non-GitHub URLs (e.g. GitLab, Bitbucket, local paths) or malformed URLs.
pub fn normalize_github_slug(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    // SCP-like SSH syntax: `git@github.com:owner/repo.git` (or `anything@github.com:owner/repo`)
    if let Some((left, path_part)) = trimmed.split_once(':') {
        if !left.contains("://") && !trimmed.contains("://") {
            let host = left
                .rsplit_once('@')
                .map(|(_, h)| h)
                .unwrap_or(left)
                .to_ascii_lowercase();
            if host == "github.com" {
                return extract_owner_repo(path_part);
            }
            return None;
        }
    }

    // Standard URL syntax: https://, http://, ssh://, git://
    let parsed = Url::parse(trimmed).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    if host != "github.com" && host != "www.github.com" {
        return None;
    }
    extract_owner_repo(parsed.path())
}

fn extract_owner_repo(path: &str) -> Option<String> {
    let trimmed = path.trim().trim_matches('/');
    let mut parts = trimmed.split('/').filter(|s| !s.is_empty());
    let owner = parts.next()?;
    let repo_raw = parts.next()?;
    // Reject extra path segments beyond owner/repo (e.g. pull/123)
    if parts.next().is_some() {
        return None;
    }
    let repo = repo_raw.strip_suffix(".git").unwrap_or(repo_raw);
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(format!(
        "{}/{}",
        owner.to_ascii_lowercase(),
        repo.to_ascii_lowercase()
    ))
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CompareOptions {
    pub exclude_archived: bool,
    pub exclude_forks: bool,
}

/// Merge local repository reports with the GitHub repository list.
///
/// - Local repos whose remotes match a GitHub repo get linked (`github_full_name`, `archived`, `fork`, etc.).
/// - Local repos that have remotes (not `NO_REMOTE`) but none match GitHub receive `Status::NotOnGithub`.
/// - GitHub repos with no local match become `Status::NotCloned` entries.
pub fn merge_local_and_github(
    local_reports: Vec<RepoReport>,
    github_repos: Vec<GithubRepo>,
    opts: CompareOptions,
) -> Vec<RepoReport> {
    // Index all GitHub repos by normalized `owner/repo`
    let mut gh_all: HashMap<String, GithubRepo> = HashMap::new();
    for repo in github_repos {
        let slug = repo.full_name.to_ascii_lowercase();
        gh_all.insert(slug, repo);
    }

    let mut matched_slugs: HashSet<String> = HashSet::new();
    let mut merged: Vec<RepoReport> = Vec::with_capacity(local_reports.len() + gh_all.len());

    for mut local in local_reports {
        let mut candidate_urls: Vec<&str> = Vec::new();
        if let Some(ref primary) = local.remote_url {
            candidate_urls.push(primary.as_str());
        }
        for r in &local.remotes {
            if !candidate_urls.contains(&r.url.as_str()) {
                candidate_urls.push(r.url.as_str());
            }
        }

        let mut matched_repo: Option<&GithubRepo> = None;
        let mut matched_slug: Option<String> = None;

        for url in candidate_urls {
            if let Some(slug) = normalize_github_slug(url) {
                if let Some(gh) = gh_all.get(&slug) {
                    matched_repo = Some(gh);
                    matched_slug = Some(slug);
                    break;
                }
            }
        }

        if let (Some(gh), Some(slug)) = (matched_repo, matched_slug) {
            matched_slugs.insert(slug);
            local.github_full_name = Some(gh.full_name.clone());
            local.clone_url = Some(gh.clone_url.clone());
            local.ssh_url = Some(gh.ssh_url.clone());
            local.archived = gh.archived;
            local.fork = gh.fork;
            if opts.exclude_archived && gh.archived {
                continue;
            }
            if opts.exclude_forks && gh.fork {
                continue;
            }
        } else if !local.has_status(Status::NoRemote) {
            // Has at least one remote, but none matched the user's/org's GitHub repos
            local.statuses.push(Status::NotOnGithub);
        }

        local.normalize_statuses();
        merged.push(local);
    }

    // Remaining GitHub repos not matched locally -> NOT_CLONED
    for (slug, gh) in gh_all {
        if matched_slugs.contains(&slug) {
            continue;
        }
        if opts.exclude_archived && gh.archived {
            continue;
        }
        if opts.exclude_forks && gh.fork {
            continue;
        }

        merged.push(RepoReport {
            name: gh.name.clone(),
            path: None,
            remote_url: Some(gh.clone_url.clone()),
            remotes: Vec::new(),
            branch: gh.default_branch.clone(),
            detached_head: false,
            is_bare: false,
            is_worktree: false,
            submodules: Vec::new(),
            ahead: None,
            behind: None,
            dirty_files: 0,
            untracked_files: 0,
            stashes: 0,
            statuses: vec![Status::NotCloned],
            last_commit: gh.pushed_at,
            github_full_name: Some(gh.full_name),
            clone_url: Some(gh.clone_url),
            ssh_url: Some(gh.ssh_url),
            archived: gh.archived,
            fork: gh.fork,
            error: None,
        });
    }

    sort_reports(&mut merged);
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn normalizes_all_github_url_variants() {
        let cases = [
            ("git@github.com:Alice/MyRepo.git", Some("alice/myrepo")),
            ("git@github.com:Alice/MyRepo", Some("alice/myrepo")),
            ("ssh://git@github.com/Alice/MyRepo.git", Some("alice/myrepo")),
            ("ssh://git@github.com:22/Alice/MyRepo.git", Some("alice/myrepo")),
            ("https://github.com/Alice/MyRepo.git", Some("alice/myrepo")),
            ("https://github.com/Alice/MyRepo", Some("alice/myrepo")),
            ("https://github.com/Alice/MyRepo/", Some("alice/myrepo")),
            ("http://github.com/Alice/MyRepo.git", Some("alice/myrepo")),
            ("git://github.com/Alice/MyRepo.git", Some("alice/myrepo")),
            (
                "https://alice:ghp_supersecret@github.com/Alice/MyRepo.git",
                Some("alice/myrepo"),
            ),
            ("git@gitlab.com:Alice/MyRepo.git", None),
            ("https://bitbucket.org/Alice/MyRepo.git", None),
            ("/local/path/repo", None),
        ];

        for (input, expected) in cases {
            assert_eq!(
                normalize_github_slug(input).as_deref(),
                expected,
                "failed for input: {input}"
            );
        }
    }

    #[test]
    fn sanitizes_credentials_from_remote_urls() {
        assert_eq!(
            sanitize_remote_url("https://alice:ghp_secret123@github.com/alice/repo.git"),
            "https://github.com/alice/repo.git"
        );
        assert_eq!(
            sanitize_remote_url("git@github.com:alice/repo.git"),
            "git@github.com:alice/repo.git"
        );
    }

    #[test]
    fn merges_local_and_github_correctly() {
        let local = vec![
            RepoReport {
                name: "matched".into(),
                path: Some(PathBuf::from("/work/matched")),
                remote_url: Some("git@github.com:Me/Matched.git".into()),
                remotes: vec![],
                branch: Some("main".into()),
                detached_head: false,
                is_bare: false,
                is_worktree: false,
                submodules: vec![],
                ahead: Some(0),
                behind: Some(0),
                dirty_files: 0,
                untracked_files: 0,
                stashes: 0,
                statuses: vec![Status::Clean],
                last_commit: None,
                github_full_name: None,
                clone_url: None,
                ssh_url: None,
                archived: false,
                fork: false,
                error: None,
            },
            RepoReport {
                name: "orphan-gitlab".into(),
                path: Some(PathBuf::from("/work/orphan-gitlab")),
                remote_url: Some("git@gitlab.com:me/orphan.git".into()),
                remotes: vec![],
                branch: Some("main".into()),
                detached_head: false,
                is_bare: false,
                is_worktree: false,
                submodules: vec![],
                ahead: Some(0),
                behind: Some(0),
                dirty_files: 0,
                untracked_files: 0,
                stashes: 0,
                statuses: vec![Status::Clean],
                last_commit: None,
                github_full_name: None,
                clone_url: None,
                ssh_url: None,
                archived: false,
                fork: false,
                error: None,
            },
        ];

        let gh = vec![
            GithubRepo {
                name: "Matched".into(),
                full_name: "Me/Matched".into(),
                clone_url: "https://github.com/Me/Matched.git".into(),
                ssh_url: "git@github.com:Me/Matched.git".into(),
                html_url: "https://github.com/Me/Matched".into(),
                archived: false,
                fork: false,
                private: true,
                default_branch: Some("main".into()),
                pushed_at: None,
            },
            GithubRepo {
                name: "cloud-only".into(),
                full_name: "Me/cloud-only".into(),
                clone_url: "https://github.com/Me/cloud-only.git".into(),
                ssh_url: "git@github.com:Me/cloud-only.git".into(),
                html_url: "https://github.com/Me/cloud-only".into(),
                archived: false,
                fork: false,
                private: false,
                default_branch: Some("main".into()),
                pushed_at: None,
            },
        ];

        let merged = merge_local_and_github(local, gh, CompareOptions::default());
        assert_eq!(merged.len(), 3);

        let orphan = merged.iter().find(|r| r.name == "orphan-gitlab").unwrap();
        assert_eq!(orphan.statuses, vec![Status::NotOnGithub]);

        let missing = merged.iter().find(|r| r.name == "cloud-only").unwrap();
        assert_eq!(missing.statuses, vec![Status::NotCloned]);
        assert!(missing.path.is_none());

        let matched = merged.iter().find(|r| r.name == "matched").unwrap();
        assert_eq!(matched.statuses, vec![Status::Clean]);
    }
}
