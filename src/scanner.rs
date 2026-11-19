use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use walkdir::{DirEntry, WalkDir};

use crate::config::Config;

/// Clean up Windows extended-length path prefix (`\\?\`) for clean display and comparisons.
pub fn normalize_display_path(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(stripped) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{}", stripped))
    } else if let Some(stripped) = s.strip_prefix(r"\\?\") {
        PathBuf::from(stripped)
    } else {
        path.to_path_buf()
    }
}

/// Normalize a relative path to forward slashes in lowercase (case-insensitive on Windows,
/// exact on Unix) for matching `skip_folders` entries.
fn normalize_rel_for_match(rel: &Path) -> String {
    let joined = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    if cfg!(windows) {
        joined.to_ascii_lowercase()
    } else {
        joined
    }
}

fn normalize_pattern(p: &str) -> String {
    let trimmed = p.trim().replace('\\', "/");
    let trimmed = trimmed.trim_matches('/');
    if cfg!(windows) {
        trimmed.to_ascii_lowercase()
    } else {
        trimmed.to_string()
    }
}

/// Returns true if `dir` is a git repository root:
/// - contains a `.git` directory (normal repo)
/// - contains a `.git` file (worktree or submodule checkout)
/// - looks like a bare git repository (`HEAD`, `objects/`, `refs/`, and `config` containing `bare = true`)
pub fn is_git_repo_dir(dir: &Path) -> bool {
    let dot_git = dir.join(".git");
    if dot_git.is_dir() || dot_git.is_file() {
        return true;
    }

    // Bare repository heuristic
    let head = dir.join("HEAD");
    let objects = dir.join("objects");
    let refs = dir.join("refs");
    let config = dir.join("config");
    if head.is_file() && objects.is_dir() && refs.is_dir() && config.is_file() {
        if let Ok(contents) = fs::read_to_string(&config) {
            if contents.lines().any(|line| {
                let l = line.trim().to_ascii_lowercase();
                l == "bare = true" || l == "bare=true"
            }) {
                return true;
            }
        }
    }

    false
}

/// Walk `config.root` up to `config.scan.max_depth` and return all discovered git repository paths.
///
/// Guarantees:
/// - Never descends into a discovered git repository (prevents double-counting submodules
///   and avoids traversing large source trees).
/// - Respects `config.scan.exclude` (matched against directory names) and
///   `config.machine.skip_folders` (matched against directory names OR relative paths from `root`).
pub fn discover_repos(config: &Config) -> Result<Vec<PathBuf>> {
    if !config.root.exists() {
        bail!("root directory does not exist: {}", config.root.display());
    }
    if !config.root.is_dir() {
        bail!("root path is not a directory: {}", config.root.display());
    }

    let root = normalize_display_path(
        &fs::canonicalize(&config.root)
            .with_context(|| format!("failed to canonicalize root {}", config.root.display()))?,
    );

    let exclude_names: HashSet<String> = config
        .scan
        .exclude
        .iter()
        .map(|s| normalize_pattern(s))
        .filter(|s| !s.is_empty())
        .collect();

    let skip_patterns: Vec<String> = config
        .machine
        .skip_folders
        .iter()
        .map(|s| normalize_pattern(s))
        .filter(|s| !s.is_empty())
        .collect();

    // If the root directory itself is a git repository, just return it.
    if is_git_repo_dir(&root) {
        return Ok(vec![root]);
    }

    let mut repos: Vec<PathBuf> = Vec::new();
    let mut walker = WalkDir::new(&root)
        .min_depth(1)
        .max_depth(config.scan.max_depth)
        .follow_links(false)
        .into_iter();

    while let Some(entry_res) = walker.next() {
        let entry: DirEntry = match entry_res {
            Ok(e) => e,
            Err(_) => continue,
        };

        if !entry.file_type().is_dir() {
            continue;
        }

        let path = entry.path();
        let name = entry.file_name().to_string_lossy();
        let norm_name = normalize_pattern(&name);

        // Always skip `.git` directories themselves
        if norm_name == ".git" {
            walker.skip_current_dir();
            continue;
        }

        // Check `scan.exclude` against folder name
        if exclude_names.contains(&norm_name) {
            walker.skip_current_dir();
            continue;
        }

        // Check `machine.skip_folders` against both folder name and relative path prefix
        if let Ok(rel) = path.strip_prefix(&root) {
            let norm_rel = normalize_rel_for_match(rel);
            let should_skip = skip_patterns.iter().any(|pat| {
                norm_name == *pat
                    || norm_rel == *pat
                    || norm_rel.starts_with(&format!("{}/", pat))
            });
            if should_skip {
                walker.skip_current_dir();
                continue;
            }
        }

        if is_git_repo_dir(path) {
            repos.push(normalize_display_path(path));
            // Do not descend into discovered git repositories.
            walker.skip_current_dir();
        }
    }

    repos.sort();
    Ok(repos)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn discovers_repos_and_respects_excludes_and_skip_folders() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        // Normal repo at depth 1
        fs::create_dir_all(root.join("alpha/.git")).unwrap();
        // Nested repo at depth 2
        fs::create_dir_all(root.join("01_Veltrion/beta/.git")).unwrap();
        // Submodule inside beta should NOT be double-discovered
        fs::create_dir_all(root.join("01_Veltrion/beta/submod/.git")).unwrap();
        // Skipped folder (company folder)
        fs::create_dir_all(root.join("02_Rainco/secret-repo/.git")).unwrap();
        // Excluded folder
        fs::create_dir_all(root.join("99_Archive/old-repo/.git")).unwrap();
        // Too deep (depth 4 when max_depth = 3)
        fs::create_dir_all(root.join("a/b/c/too-deep/.git")).unwrap();
        // Worktree (`.git` is a file)
        fs::create_dir_all(root.join("wt-repo")).unwrap();
        fs::write(root.join("wt-repo/.git"), "gitdir: ../alpha/.git/worktrees/wt\n").unwrap();

        let mut cfg = Config::from_root(root.to_path_buf());
        cfg.scan.max_depth = 3;
        cfg.scan.exclude = vec!["99_Archive".into()];
        cfg.machine.skip_folders = vec!["02_Rainco".into()];

        let found = discover_repos(&cfg).unwrap();
        let mut names: Vec<String> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        names.sort();

        assert_eq!(names, vec!["alpha", "beta", "wt-repo"]);
    }
}
