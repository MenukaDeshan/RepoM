use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::tempdir;

use repom::actions::pull_clean_repos;
use repom::config::Config;
use repom::local::inspect_repos_parallel;
use repom::model::Status;
use repom::scanner::discover_repos;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test User")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test User")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("failed to execute git");
    assert!(
        out.status.success(),
        "git {:?} failed in {}:\nstdout: {}\nstderr: {}",
        args,
        dir.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn init_bare_origin(path: &Path) {
    fs::create_dir_all(path).unwrap();
    git(path, &["init", "--bare", "-b", "main"]);
}

fn clone_and_seed(origin: &Path, work_dir: &Path) {
    git(
        work_dir.parent().unwrap(),
        &[
            "clone",
            &origin.to_string_lossy(),
            &work_dir.to_string_lossy(),
        ],
    );
    git(work_dir, &["config", "user.name", "Test User"]);
    git(work_dir, &["config", "user.email", "test@example.com"]);
    git(work_dir, &["checkout", "-B", "main"]);
    fs::write(work_dir.join("README.md"), "# seed\n").unwrap();
    git(work_dir, &["add", "README.md"]);
    git(work_dir, &["commit", "-m", "initial commit"]);
    git(work_dir, &["push", "-u", "origin", "main"]);
}

#[tokio::test]
async fn scans_diverse_local_repo_states_and_pulls_clean_behind_only() {
    let tmp = tempdir().unwrap();
    let origins = tmp.path().join("origins");
    let work = tmp.path().join("work");
    fs::create_dir_all(&origins).unwrap();
    fs::create_dir_all(&work).unwrap();

    // 1. Clean repo
    let clean_origin = origins.join("clean.git");
    init_bare_origin(&clean_origin);
    let clean_dir = work.join("clean-repo");
    clone_and_seed(&clean_origin, &clean_dir);

    // 2. Dirty + Untracked repo
    let dirty_origin = origins.join("dirty.git");
    init_bare_origin(&dirty_origin);
    let dirty_dir = work.join("dirty-repo");
    clone_and_seed(&dirty_origin, &dirty_dir);
    fs::write(dirty_dir.join("README.md"), "# modified\n").unwrap();
    fs::write(dirty_dir.join("new_file.txt"), "untracked\n").unwrap();

    // 3. Ahead repo
    let ahead_origin = origins.join("ahead.git");
    init_bare_origin(&ahead_origin);
    let ahead_dir = work.join("ahead-repo");
    clone_and_seed(&ahead_origin, &ahead_dir);
    fs::write(ahead_dir.join("local.txt"), "unpushed\n").unwrap();
    git(&ahead_dir, &["add", "local.txt"]);
    git(&ahead_dir, &["commit", "-m", "local commit"]);

    // 4. Behind repo (clean)
    let behind_origin = origins.join("behind.git");
    init_bare_origin(&behind_origin);
    let behind_dir = work.join("behind-repo");
    clone_and_seed(&behind_origin, &behind_dir);
    // Push a commit from a second clone so behind_dir falls behind
    let helper_clone = origins.join("behind-pusher");
    git(
        &origins,
        &[
            "clone",
            &behind_origin.to_string_lossy(),
            &helper_clone.to_string_lossy(),
        ],
    );
    git(&helper_clone, &["config", "user.name", "Test User"]);
    git(&helper_clone, &["config", "user.email", "test@example.com"]);
    fs::write(helper_clone.join("remote.txt"), "from remote\n").unwrap();
    git(&helper_clone, &["add", "remote.txt"]);
    git(&helper_clone, &["commit", "-m", "remote commit"]);
    git(&helper_clone, &["push", "origin", "main"]);
    git(&behind_dir, &["fetch", "origin"]);

    // 5. Diverged repo (behind AND ahead)
    let div_origin = origins.join("diverged.git");
    init_bare_origin(&div_origin);
    let div_dir = work.join("diverged-repo");
    clone_and_seed(&div_origin, &div_dir);
    let div_pusher = origins.join("div-pusher");
    git(
        &origins,
        &[
            "clone",
            &div_origin.to_string_lossy(),
            &div_pusher.to_string_lossy(),
        ],
    );
    git(&div_pusher, &["config", "user.name", "Test User"]);
    git(&div_pusher, &["config", "user.email", "test@example.com"]);
    fs::write(div_pusher.join("remote_div.txt"), "remote\n").unwrap();
    git(&div_pusher, &["add", "remote_div.txt"]);
    git(&div_pusher, &["commit", "-m", "remote div"]);
    git(&div_pusher, &["push", "origin", "main"]);
    git(&div_dir, &["fetch", "origin"]);
    fs::write(div_dir.join("local_div.txt"), "local\n").unwrap();
    git(&div_dir, &["add", "local_div.txt"]);
    git(&div_dir, &["commit", "-m", "local div"]);

    // 6. No remote repo
    let no_remote_dir = work.join("no-remote-repo");
    fs::create_dir_all(&no_remote_dir).unwrap();
    git(&no_remote_dir, &["init", "-b", "main"]);
    git(&no_remote_dir, &["config", "user.name", "Test User"]);
    git(&no_remote_dir, &["config", "user.email", "test@example.com"]);
    fs::write(no_remote_dir.join("a.txt"), "hello\n").unwrap();
    git(&no_remote_dir, &["add", "a.txt"]);
    git(&no_remote_dir, &["commit", "-m", "init"]);

    // 7. No upstream repo (has origin, branch not tracking)
    let no_up_dir = work.join("no-upstream-repo");
    fs::create_dir_all(&no_up_dir).unwrap();
    git(&no_up_dir, &["init", "-b", "dev"]);
    git(&no_up_dir, &["config", "user.name", "Test User"]);
    git(&no_up_dir, &["config", "user.email", "test@example.com"]);
    git(
        &no_up_dir,
        &["remote", "add", "origin", "https://github.com/me/no-upstream.git"],
    );
    fs::write(no_up_dir.join("a.txt"), "hello\n").unwrap();
    git(&no_up_dir, &["add", "a.txt"]);
    git(&no_up_dir, &["commit", "-m", "init"]);

    // 8. Stashed repo
    let stash_origin = origins.join("stash.git");
    init_bare_origin(&stash_origin);
    let stash_dir = work.join("stashed-repo");
    clone_and_seed(&stash_origin, &stash_dir);
    fs::write(stash_dir.join("README.md"), "# stashed edit\n").unwrap();
    git(&stash_dir, &["stash", "push", "-m", "wip"]);

    // 9. Detached HEAD repo
    let detached_origin = origins.join("detached.git");
    init_bare_origin(&detached_origin);
    let detached_dir = work.join("detached-repo");
    clone_and_seed(&detached_origin, &detached_dir);
    git(&detached_dir, &["checkout", "--detach", "HEAD"]);

    // 10. Skipped company folder (must NEVER appear in reports)
    let skipped_dir = work.join("02_Rainco").join("secret-corp");
    fs::create_dir_all(&skipped_dir).unwrap();
    git(&skipped_dir, &["init", "-b", "main"]);

    let mut cfg = Config::from_root(work.clone());
    cfg.machine.skip_folders = vec!["02_Rainco".into()];

    let discovered: Vec<PathBuf> = discover_repos(&cfg).unwrap();
    let reports = inspect_repos_parallel(&discovered);

    // Ensure skipped folder is completely absent
    assert!(
        !reports.iter().any(|r| r.name == "secret-corp"),
        "skipped folder leaked into scan results"
    );

    let find = |name: &str| {
        reports
            .iter()
            .find(|r| r.name == name)
            .unwrap_or_else(|| panic!("missing report for {name}"))
    };

    assert_eq!(find("clean-repo").statuses, vec![Status::Clean]);

    let dirty = find("dirty-repo");
    assert!(dirty.has_status(Status::Dirty));
    assert!(dirty.has_status(Status::Untracked));
    assert_eq!(dirty.dirty_files, 1);
    assert_eq!(dirty.untracked_files, 1);

    let ahead = find("ahead-repo");
    assert_eq!(ahead.statuses, vec![Status::Ahead]);
    assert_eq!(ahead.ahead, Some(1));
    assert_eq!(ahead.behind, Some(0));

    let behind = find("behind-repo");
    assert_eq!(behind.statuses, vec![Status::Behind]);
    assert_eq!(behind.ahead, Some(0));
    assert_eq!(behind.behind, Some(1));
    assert!(behind.is_clean_pullable());

    let diverged = find("diverged-repo");
    assert_eq!(diverged.statuses, vec![Status::Diverged]);
    assert_eq!(diverged.ahead, Some(1));
    assert_eq!(diverged.behind, Some(1));
    assert!(!diverged.is_clean_pullable());

    let no_remote = find("no-remote-repo");
    assert_eq!(no_remote.statuses, vec![Status::NoRemote]);

    let no_up = find("no-upstream-repo");
    assert_eq!(no_up.statuses, vec![Status::NoUpstream]);
    assert_eq!(no_up.ahead, None);
    assert_eq!(no_up.behind, None);

    let stashed = find("stashed-repo");
    assert_eq!(stashed.statuses, vec![Status::Stashed]);
    assert_eq!(stashed.stashes, 1);

    let detached = find("detached-repo");
    assert!(detached.detached_head);
    assert!(detached.branch.as_deref().unwrap_or("").starts_with("detached@"));

    // Phase 4 pull: dry-run should NOT modify behind-repo (and skips diverged-repo)
    let dry_outcomes = pull_clean_repos(&reports, false, 30).await;
    assert_eq!(dry_outcomes.len(), 2);
    assert!(dry_outcomes.iter().all(|o| !o.applied));
    assert!(!behind_dir.join("remote.txt").exists());

    // Phase 4 pull: apply should fast-forward behind-repo cleanly and skip diverged-repo
    let apply_outcomes = pull_clean_repos(&reports, true, 30).await;
    let pulled = apply_outcomes
        .iter()
        .find(|o| o.repo_name == "behind-repo")
        .unwrap();
    assert!(pulled.applied);
    assert!(pulled.success);
    assert!(behind_dir.join("remote.txt").exists());

    let skipped = apply_outcomes
        .iter()
        .find(|o| o.repo_name == "diverged-repo")
        .unwrap();
    assert!(!skipped.applied);
    assert!(skipped.message.starts_with("SKIPPED"));

    // Re-inspect behind-repo: should now be CLEAN
    let after = inspect_repos_parallel(&[behind_dir]);
    assert_eq!(after[0].statuses, vec![Status::Clean]);
}
