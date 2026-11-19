use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

use crate::model::{RepoReport, ScanOutput, Status, Summary};

/// Filter reports by one or more statuses if supplied.
pub fn filter_reports(reports: Vec<RepoReport>, only: &[Status]) -> Vec<RepoReport> {
    if only.is_empty() {
        return reports;
    }
    reports
        .into_iter()
        .filter(|r| only.iter().any(|st| r.has_status(*st)))
        .collect()
}

/// Format a concise human-readable summary line:
/// `Summary: 52 repos | 41 clean | 6 dirty | 3 ahead | 2 behind | 1 not cloned`
pub fn format_summary_line(summary: &Summary) -> String {
    let mut parts = vec![
        format!("{} repos", summary.total),
        format!("{} clean", summary.clean),
    ];
    if summary.dirty > 0 {
        parts.push(format!("{} dirty", summary.dirty));
    }
    if summary.untracked > 0 {
        parts.push(format!("{} untracked", summary.untracked));
    }
    if summary.diverged > 0 {
        parts.push(format!("{} diverged", summary.diverged));
    }
    if summary.ahead > 0 {
        parts.push(format!("{} ahead", summary.ahead));
    }
    if summary.behind > 0 {
        parts.push(format!("{} behind", summary.behind));
    }
    if summary.stashed > 0 {
        parts.push(format!("{} stashed", summary.stashed));
    }
    if summary.no_upstream > 0 {
        parts.push(format!("{} no upstream", summary.no_upstream));
    }
    if summary.no_remote > 0 {
        parts.push(format!("{} no remote", summary.no_remote));
    }
    if summary.not_on_github > 0 {
        parts.push(format!("{} not on github", summary.not_on_github));
    }
    if summary.not_cloned > 0 {
        parts.push(format!("{} not cloned", summary.not_cloned));
    }
    if summary.errors > 0 {
        parts.push(format!("{} errors", summary.errors));
    }

    format!("Summary: {}", parts.join(" | "))
}

fn format_short_date(dt: Option<DateTime<Utc>>) -> String {
    match dt {
        Some(ts) => ts.format("%Y-%m-%d").to_string(),
        None => "-".to_string(),
    }
}

/// Render a structured JSON report string.
pub fn render_json(output: &ScanOutput) -> Result<String> {
    serde_json::to_string_pretty(output).context("failed to serialize JSON report")
}

/// Render a GitHub-flavored Markdown report suitable for Second Brain notes or logs.
pub fn render_markdown(output: &ScanOutput) -> String {
    let mut md = String::new();
    md.push_str("# RepoM Sync Report\n\n");
    md.push_str(&format!(
        "- **Machine:** `{}`\n- **Root:** `{}`\n- **Generated:** `{}`\n- **{}**\n\n",
        escape_md_inline(&output.machine),
        escape_md_inline(&output.root),
        output.generated_at.to_rfc3339(),
        format_summary_line(&output.summary),
    ));

    if output.repos.is_empty() {
        md.push_str("_No matching repositories._\n");
        return md;
    }

    md.push_str(
        "| Repo | Branch | Status | Ahead | Behind | Dirty | Untracked | Stash | Last Commit | Remote |\n",
    );
    md.push_str("|---|---|---|---:|---:|---:|---:|---:|---|---|\n");

    for r in &output.repos {
        let is_not_cloned = r.has_status(Status::NotCloned);
        let branch = if is_not_cloned {
            "-".to_string()
        } else {
            r.branch.clone().unwrap_or_else(|| "-".to_string())
        };
        let status = if let Some(ref err) = r.error {
            format!("ERROR: {}", err)
        } else {
            r.statuses
                .iter()
                .map(Status::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        };
        let ahead = r.ahead.map(|n| n.to_string()).unwrap_or_else(|| "-".into());
        let behind = r.behind.map(|n| n.to_string()).unwrap_or_else(|| "-".into());
        let dirty = if is_not_cloned {
            "-".into()
        } else {
            r.dirty_files.to_string()
        };
        let untracked = if is_not_cloned {
            "-".into()
        } else {
            r.untracked_files.to_string()
        };
        let stash = if is_not_cloned {
            "-".into()
        } else {
            r.stashes.to_string()
        };
        let last_commit = format_short_date(r.last_commit);
        let remote = r.remote_url.clone().unwrap_or_else(|| "-".into());

        md.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            escape_md_cell(&r.name),
            escape_md_cell(&branch),
            escape_md_cell(&status),
            ahead,
            behind,
            dirty,
            untracked,
            stash,
            last_commit,
            escape_md_cell(&remote),
        ));
    }

    md
}

fn escape_md_cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\r', "").replace('\n', " ")
}

fn escape_md_inline(s: &str) -> String {
    s.replace('`', "'").replace('\r', "").replace('\n', " ")
}

/// Write a report string safely to disk, creating parent directories if necessary.
pub fn write_report_file(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create output directory {}", parent.display())
            })?;
        }
    }
    fs::write(path, content)
        .with_context(|| format!("failed to write report to {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_line_formats_only_nonzero_issues() {
        let summary = Summary {
            total: 52,
            clean: 41,
            dirty: 6,
            ahead: 3,
            behind: 2,
            not_cloned: 1,
            ..Default::default()
        };
        assert_eq!(
            format_summary_line(&summary),
            "Summary: 52 repos | 41 clean | 6 dirty | 3 ahead | 2 behind | 1 not cloned"
        );
    }

    #[test]
    fn markdown_escapes_pipes_and_newlines() {
        assert_eq!(escape_md_cell("a|b\nc"), "a\\|b c");
    }
}
