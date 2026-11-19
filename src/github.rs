use std::collections::HashMap;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, TimeZone, Utc};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, LINK, USER_AGENT};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use url::Url;

const DEFAULT_API_BASE: &str = "https://api.github.com";

/// Subset of GitHub repository metadata required for sync comparison and cloning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubRepo {
    pub name: String,
    pub full_name: String,
    pub clone_url: String,
    pub ssh_url: String,
    pub html_url: String,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub fork: bool,
    #[serde(default)]
    pub private: bool,
    #[serde(default)]
    pub default_branch: Option<String>,
    #[serde(default)]
    pub pushed_at: Option<DateTime<Utc>>,
}

pub struct GithubClient {
    http: Client,
    base_url: Url,
    has_token: bool,
}

impl GithubClient {
    /// Construct a GitHub client against `https://api.github.com`.
    pub fn new(token: Option<&str>) -> Result<Self> {
        Self::with_base_url(DEFAULT_API_BASE, token)
    }

    /// Construct a GitHub client against a custom base URL.
    /// Enforces HTTPS for all non-loopback hosts so tokens are never transmitted in plaintext.
    pub fn with_base_url(base_url: &str, token: Option<&str>) -> Result<Self> {
        let parsed = Url::parse(base_url)
            .with_context(|| format!("invalid GitHub API base URL: {}", base_url))?;

        let is_loopback = matches!(parsed.host_str(), Some("127.0.0.1") | Some("localhost") | Some("[::1]"));
        if parsed.scheme() != "https" && !(parsed.scheme() == "http" && is_loopback) {
            bail!(
                "security error: GitHub API base URL must use HTTPS (got '{}')",
                parsed.scheme()
            );
        }

        let mut headers = HeaderMap::new();
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static("repom-cli/0.1.0"),
        );
        headers.insert(
            "X-GitHub-Api-Version",
            HeaderValue::from_static("2022-11-28"),
        );

        let mut has_token = false;
        if let Some(tok) = token.map(str::trim).filter(|t| !t.is_empty()) {
            let auth_val = HeaderValue::from_str(&format!("Bearer {}", tok))
                .context("GitHub token contains invalid HTTP header characters")?;
            headers.insert(AUTHORIZATION, auth_val);
            has_token = true;
        }

        let http = Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .context("failed to build HTTP client")?;

        Ok(Self {
            http,
            base_url: parsed,
            has_token,
        })
    }

    /// Fetch all repositories for the configured user (and any configured organizations),
    /// paginating 100 items per page and deduplicating by lowercase `full_name`.
    pub async fn list_all_repos(
        &self,
        github_user: Option<&str>,
        github_orgs: &[String],
    ) -> Result<Vec<GithubRepo>> {
        let mut by_slug: HashMap<String, GithubRepo> = HashMap::new();

        if self.has_token {
            // Authenticated `/user/repos` returns private + public repos owned by or shared with the user.
            let url = self
                .base_url
                .join("/user/repos?per_page=100&affiliation=owner,collaborator,organization_member&sort=updated")?;
            let user_repos = self.fetch_paginated(url).await?;
            for repo in user_repos {
                // If github_user is set and no orgs are specified, keep repos owned by github_user
                // or any configured org (plus collaborator repos if user matches).
                if let Some(user) = github_user {
                    let owner = repo
                        .full_name
                        .split('/')
                        .next()
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    let matches_user = owner == user.to_ascii_lowercase();
                    let matches_org = github_orgs
                        .iter()
                        .any(|o| o.to_ascii_lowercase() == owner);
                    if !matches_user && !matches_org {
                        continue;
                    }
                }
                by_slug.insert(repo.full_name.to_ascii_lowercase(), repo);
            }
        } else if let Some(user) = github_user {
            validate_github_segment(user, "github_user")?;
            let url = self
                .base_url
                .join(&format!("/users/{}/repos?per_page=100&type=owner", user))?;
            let user_repos = self.fetch_paginated(url).await?;
            for repo in user_repos {
                by_slug.insert(repo.full_name.to_ascii_lowercase(), repo);
            }
        } else {
            bail!(
                "GitHub authentication or `github_user` is required. \
                 Export your token in the configured `token_env` (e.g. GITHUB_TOKEN) or set `github_user` in config.toml."
            );
        }

        // Explicitly fetch each configured org as well (covers org repos that may not appear under affiliation)
        for org in github_orgs {
            validate_github_segment(org, "github_orgs entry")?;
            let url = self
                .base_url
                .join(&format!("/orgs/{}/repos?per_page=100&type=all", org))?;
            let org_repos = self.fetch_paginated(url).await?;
            for repo in org_repos {
                by_slug.insert(repo.full_name.to_ascii_lowercase(), repo);
            }
        }

        let mut list: Vec<GithubRepo> = by_slug.into_values().collect();
        list.sort_by(|a, b| {
            a.full_name
                .to_ascii_lowercase()
                .cmp(&b.full_name.to_ascii_lowercase())
        });
        Ok(list)
    }

    async fn fetch_paginated(&self, initial_url: Url) -> Result<Vec<GithubRepo>> {
        let mut results: Vec<GithubRepo> = Vec::new();
        let mut next_url: Option<Url> = Some(initial_url);
        let mut pages_fetched = 0usize;
        const MAX_PAGES: usize = 100; // Safety bound: 10,000 repos

        while let Some(url) = next_url.take() {
            pages_fetched += 1;
            if pages_fetched > MAX_PAGES {
                bail!("exceeded maximum pagination limit ({} pages) while querying GitHub API", MAX_PAGES);
            }

            // Security check: ensure pagination links never redirect our Bearer token to a different origin.
            if url.origin() != self.base_url.origin() {
                bail!(
                    "security error: refusing to follow GitHub pagination link to different origin ({})",
                    url.origin().ascii_serialization()
                );
            }

            let resp = self
                .http
                .get(url.clone())
                .send()
                .await
                .with_context(|| format!("GitHub API request failed for {}", url.path()))?;

            let status = resp.status();
            let headers = resp.headers().clone();

            if status == StatusCode::UNAUTHORIZED {
                bail!(
                    "GitHub API returned 401 Unauthorized. Check that your token in the configured environment variable is valid and not expired."
                );
            }

            if status == StatusCode::FORBIDDEN || status == StatusCode::TOO_MANY_REQUESTS {
                let remaining = headers
                    .get("x-ratelimit-remaining")
                    .and_then(|v| v.to_str().ok());
                let reset_msg = format_rate_limit_reset(&headers);
                if status == StatusCode::TOO_MANY_REQUESTS || remaining == Some("0") {
                    bail!(
                        "GitHub API rate limit exceeded ({}). {}",
                        status.as_u16(),
                        reset_msg
                    );
                }
                bail!(
                    "GitHub API returned 403 Forbidden for {}. Ensure your token has read-only repository metadata permissions.",
                    url.path()
                );
            }

            if status == StatusCode::NOT_FOUND {
                bail!(
                    "GitHub API returned 404 Not Found for {}. Check `github_user` and `github_orgs` in your config.",
                    url.path()
                );
            }

            if !status.is_success() {
                bail!("GitHub API returned unexpected HTTP {} for {}", status.as_u16(), url.path());
            }

            let page: Vec<GithubRepo> = resp
                .json()
                .await
                .with_context(|| format!("failed to parse GitHub API JSON response from {}", url.path()))?;

            results.extend(page);

            if let Some(link_val) = headers.get(LINK).and_then(|v| v.to_str().ok()) {
                if let Some(next_str) = parse_next_link(link_val) {
                    next_url = Some(
                        Url::parse(&next_str)
                            .with_context(|| format!("invalid Link next URL: {}", next_str))?,
                    );
                }
            }
        }

        Ok(results)
    }
}

/// Validate a GitHub username or organization slug to prevent path injection.
fn validate_github_segment(seg: &str, label: &str) -> Result<()> {
    if seg.is_empty()
        || seg.len() > 100
        || !seg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        || seg == "."
        || seg == ".."
    {
        bail!("invalid {} '{}': must contain only alphanumeric, '-', '_', or '.' characters", label, seg);
    }
    Ok(())
}

/// Parse RFC 5988 `Link` header and return the URL whose `rel="next"`.
pub fn parse_next_link(header: &str) -> Option<String> {
    for part in header.split(',') {
        let mut segments = part.split(';');
        let url_part = segments.next()?.trim();
        let is_next = segments.any(|param| {
            let p = param.trim();
            p == r#"rel="next""# || p == "rel=next"
        });
        if is_next && url_part.starts_with('<') && url_part.ends_with('>') {
            return Some(url_part[1..url_part.len() - 1].to_string());
        }
    }
    None
}

fn format_rate_limit_reset(headers: &HeaderMap) -> String {
    if let Some(retry_after) = headers.get("retry-after").and_then(|v| v.to_str().ok()) {
        return format!("Retry after {} seconds.", retry_after);
    }
    if let Some(reset_secs) = headers
        .get("x-ratelimit-reset")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<i64>().ok())
    {
        if let Some(dt) = Utc.timestamp_opt(reset_secs, 0).single() {
            return format!("Rate limit resets at {}.", dt.to_rfc3339());
        }
    }
    "Try again later or use an authenticated token (5000 req/hr).".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_github_link_header_next() {
        let header = r#"<https://api.github.com/user/repos?per_page=100&page=2>; rel="next", <https://api.github.com/user/repos?per_page=100&page=5>; rel="last""#;
        assert_eq!(
            parse_next_link(header).as_deref(),
            Some("https://api.github.com/user/repos?per_page=100&page=2")
        );

        let last_page = r#"<https://api.github.com/user/repos?per_page=100&page=1>; rel="prev""#;
        assert_eq!(parse_next_link(last_page), None);
    }

    #[test]
    fn rejects_non_https_remote_base_url() {
        assert!(GithubClient::with_base_url("http://api.github.com", Some("tok")).is_err());
        assert!(GithubClient::with_base_url("http://127.0.0.1:8080", Some("tok")).is_ok());
        assert!(GithubClient::with_base_url("https://api.github.com", Some("tok")).is_ok());
    }
}
