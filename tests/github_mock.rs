use std::path::PathBuf;

use serde_json::json;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use repom::compare::{merge_local_and_github, CompareOptions};
use repom::github::GithubClient;
use repom::model::{RepoReport, Status};

fn sample_gh_repo(owner: &str, name: &str, archived: bool, fork: bool) -> serde_json::Value {
    json!({
        "name": name,
        "full_name": format!("{owner}/{name}"),
        "clone_url": format!("https://github.com/{owner}/{name}.git"),
        "ssh_url": format!("git@github.com:{owner}/{name}.git"),
        "html_url": format!("https://github.com/{owner}/{name}"),
        "archived": archived,
        "fork": fork,
        "private": true,
        "default_branch": "main",
        "pushed_at": "2026-10-01T12:00:00Z"
    })
}

#[tokio::test]
async fn paginates_user_and_org_repos_and_reconciles_with_local() {
    // wiremock binds to 127.0.0.1 (never 0.0.0.0)
    let server = MockServer::start().await;
    let base = server.uri();

    let page2_url = format!("{}/user/repos?per_page=100&page=2", base);
    let link_header = format!(r#"<{}>; rel="next""#, page2_url);

    // Page 1 of /user/repos
    Mock::given(method("GET"))
        .and(path("/user/repos"))
        .and(header("authorization", "Bearer test-secret-token"))
        .and(query_param("per_page", "100"))
        .and(query_param(
            "affiliation",
            "owner,collaborator,organization_member",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("link", link_header.as_str())
                .set_body_json(json!([sample_gh_repo("alice", "alpha", false, false)])),
        )
        .mount(&server)
        .await;

    // Page 2 of /user/repos
    Mock::given(method("GET"))
        .and(path("/user/repos"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            sample_gh_repo("alice", "cloud-only", false, false),
            sample_gh_repo("alice", "old-archive", true, false),
        ])))
        .mount(&server)
        .await;

    // Org repos (including a duplicate that should deduplicate cleanly)
    Mock::given(method("GET"))
        .and(path("/orgs/veltrion/repos"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            sample_gh_repo("veltrion", "core-api", false, false),
            sample_gh_repo("veltrion", "core-api", false, false),
        ])))
        .mount(&server)
        .await;

    let client = GithubClient::with_base_url(&base, Some("test-secret-token")).unwrap();
    let repos = client
        .list_all_repos(Some("alice"), &["veltrion".to_string()])
        .await
        .unwrap();

    assert_eq!(repos.len(), 4);

    // Local has `alpha` (matching SSH URL) and `local-only-gitlab` (not on GitHub)
    let local = vec![
        RepoReport {
            name: "alpha".into(),
            path: Some(PathBuf::from("/work/alpha")),
            remote_url: Some("git@github.com:Alice/Alpha.git".into()),
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
            name: "local-only-gitlab".into(),
            path: Some(PathBuf::from("/work/local-only-gitlab")),
            remote_url: Some("https://gitlab.com/alice/private.git".into()),
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

    let merged = merge_local_and_github(
        local,
        repos,
        CompareOptions {
            exclude_archived: true,
            exclude_forks: false,
        },
    );

    // old-archive excluded => alpha (CLEAN), local-only-gitlab (NOT_ON_GITHUB), cloud-only (NOT_CLONED), core-api (NOT_CLONED)
    assert_eq!(merged.len(), 4);
    assert!(merged.iter().all(|r| r.name != "old-archive"));

    let alpha = merged.iter().find(|r| r.name == "alpha").unwrap();
    assert_eq!(alpha.statuses, vec![Status::Clean]);

    let orphan = merged
        .iter()
        .find(|r| r.name == "local-only-gitlab")
        .unwrap();
    assert_eq!(orphan.statuses, vec![Status::NotOnGithub]);

    let not_cloned: Vec<&str> = merged
        .iter()
        .filter(|r| r.has_status(Status::NotCloned))
        .map(|r| r.name.as_str())
        .collect();
    assert_eq!(not_cloned, vec!["cloud-only", "core-api"]);
}

#[tokio::test]
async fn handles_rate_limit_without_leaking_token() {
    let server = MockServer::start().await;
    let secret = "ghp_super_secret_token_value";

    Mock::given(method("GET"))
        .and(path("/user/repos"))
        .respond_with(
            ResponseTemplate::new(403)
                .insert_header("x-ratelimit-remaining", "0")
                .insert_header("x-ratelimit-reset", "1795000000"),
        )
        .mount(&server)
        .await;

    let client = GithubClient::with_base_url(&server.uri(), Some(secret)).unwrap();
    let err = client.list_all_repos(Some("alice"), &[]).await.unwrap_err();
    let msg = format!("{:#}", err);

    assert!(msg.contains("rate limit exceeded"));
    assert!(!msg.contains(secret), "token must never appear in error output");
}

#[tokio::test]
async fn blocks_cross_origin_pagination_link_redirect() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/user/repos"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header(
                    "link",
                    r#"<https://evil.example.com/steal-token>; rel="next""#,
                )
                .set_body_json(json!([])),
        )
        .mount(&server)
        .await;

    let client = GithubClient::with_base_url(&server.uri(), Some("tok")).unwrap();
    let err = client.list_all_repos(Some("alice"), &[]).await.unwrap_err();
    assert!(err.to_string().contains("different origin"));
}
