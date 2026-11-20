use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

use chrono::{DateTime, Local, Utc};
use eframe::egui::{
    self, Align, Color32, FontData, FontDefinitions, FontFamily, FontId, Layout, Margin, Rect,
    RichText, Rounding, Sense, Stroke, TextStyle, Vec2,
};
use egui_extras::{Column, TableBuilder};

use crate::actions::{
    clone_missing_repos, fetch_repos_parallel, open_github_url, open_local_target, pull_clean_repos,
};
use crate::compare::{merge_local_and_github, CompareOptions};
use crate::config::Config;
use crate::github::GithubClient;
use crate::local::inspect_repos_with_progress;
use crate::model::{RepoReport, ScanOutput, Status, Summary};
use crate::report::{format_summary_line, render_json, render_markdown, write_report_file};
use crate::scanner::discover_repos;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusFilter {
    All,
    OutOfSync,
    NeedsAttention,
    Exact(Status),
}

impl StatusFilter {
    fn title(self) -> &'static str {
        match self {
            StatusFilter::All => "All Repositories",
            StatusFilter::OutOfSync => "Out of Sync",
            StatusFilter::NeedsAttention => "Needs Commit / Push",
            StatusFilter::Exact(Status::Clean) => "Clean & Synced",
            StatusFilter::Exact(Status::Dirty) => "Dirty Working Tree",
            StatusFilter::Exact(Status::Untracked) => "Untracked Files",
            StatusFilter::Exact(Status::Ahead) => "Ahead of Upstream",
            StatusFilter::Exact(Status::Behind) => "Behind Upstream",
            StatusFilter::Exact(Status::Diverged) => "Diverged Branches",
            StatusFilter::Exact(Status::Stashed) => "Local Stashes",
            StatusFilter::Exact(Status::NoUpstream) => "No Upstream Branch",
            StatusFilter::Exact(Status::NoRemote) => "No Remote Configured",
            StatusFilter::Exact(Status::NotOnGithub) => "Not on GitHub",
            StatusFilter::Exact(Status::NotCloned) => "Not Cloned Locally",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortMode {
    Urgency,
    Name,
    LastCommit,
    DirtyFiles,
}

impl SortMode {
    fn label(self) -> &'static str {
        match self {
            SortMode::Urgency => "Urgency (worst first)",
            SortMode::Name => "Name (A-Z)",
            SortMode::LastCommit => "Last Commit (newest)",
            SortMode::DirtyFiles => "Changes (most first)",
        }
    }
}

#[derive(Debug, Clone)]
enum PendingConfirm {
    PullOne { repo: RepoReport },
    PullAllClean { repos: Vec<RepoReport> },
    CloneOne { repo: RepoReport, use_ssh: bool },
    CloneAllMissing { repos: Vec<RepoReport>, use_ssh: bool },
}

#[derive(Debug)]
enum BgEvent {
    ScanDiscovered(usize),
    ScanInspected(Box<RepoReport>),
    ScanFetching,
    ScanComparing,
    ScanDone {
        reports: Vec<RepoReport>,
        github_enabled: bool,
        github_error: Option<String>,
        toast: String,
    },
    ActionDone {
        success: bool,
        toast: String,
        reports: Vec<RepoReport>,
        github_enabled: bool,
        github_error: Option<String>,
    },
    Error(String),
}

struct SettingsDraft {
    root: String,
    github_user: String,
    github_orgs: String,
    token_env: String,
    max_depth: String,
    exclude: String,
    machine_name: String,
    skip_folders: String,
}

impl SettingsDraft {
    fn from_config(cfg: &Config) -> Self {
        Self {
            root: cfg.root.display().to_string(),
            github_user: cfg.github_user.clone().unwrap_or_default(),
            github_orgs: cfg.github_orgs.join(", "),
            token_env: cfg.token_env.clone(),
            max_depth: cfg.scan.max_depth.to_string(),
            exclude: cfg.scan.exclude.join(", "),
            machine_name: cfg.machine.name.clone(),
            skip_folders: cfg.machine.skip_folders.join(", "),
        }
    }

    fn apply_to_config(&self, base: &Config) -> Result<Config, String> {
        let mut next = base.clone();
        let root_trimmed = self.root.trim();
        if root_trimmed.is_empty() {
            return Err("Root directory cannot be empty.".to_string());
        }
        next.root = PathBuf::from(root_trimmed);
        next.github_user = {
            let u = self.github_user.trim();
            if u.is_empty() {
                None
            } else {
                Some(u.to_string())
            }
        };
        next.github_orgs = split_csv(&self.github_orgs);
        next.token_env = self.token_env.trim().to_string();
        next.scan.max_depth = self
            .max_depth
            .trim()
            .parse::<usize>()
            .map_err(|_| "Max depth must be a positive integer.".to_string())?;
        next.scan.exclude = split_csv(&self.exclude);
        next.machine.name = {
            let m = self.machine_name.trim();
            if m.is_empty() {
                "unknown".to_string()
            } else {
                m.to_string()
            }
        };
        next.machine.skip_folders = split_csv(&self.skip_folders);
        next.validate().map_err(|e| e.to_string())?;
        Ok(next)
    }
}

fn split_csv(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(String::from)
        .collect()
}

#[derive(Clone, Copy)]
struct Palette {
    sidebar: Color32,
    canvas: Color32,
    surface: Color32,
    elevated: Color32,
    hover: Color32,
    border: Color32,
    border_subtle: Color32,
    text: Color32,
    text_secondary: Color32,
    muted: Color32,
    accent: Color32,
    accent_soft: Color32,
    green: Color32,
    green_soft: Color32,
    red: Color32,
    red_soft: Color32,
    amber: Color32,
    amber_soft: Color32,
    purple: Color32,
    purple_soft: Color32,
    cyan: Color32,
    cyan_soft: Color32,
}

impl Palette {
    fn dark() -> Self {
        Self {
            sidebar: Color32::from_rgb(13, 17, 23),
            canvas: Color32::from_rgb(17, 22, 31),
            surface: Color32::from_rgb(22, 28, 39),
            elevated: Color32::from_rgb(28, 36, 51),
            hover: Color32::from_rgb(33, 43, 61),
            border: Color32::from_rgb(42, 53, 72),
            border_subtle: Color32::from_rgb(31, 40, 56),
            text: Color32::from_rgb(230, 237, 243),
            text_secondary: Color32::from_rgb(173, 186, 199),
            muted: Color32::from_rgb(125, 138, 156),
            accent: Color32::from_rgb(47, 129, 247),
            accent_soft: Color32::from_rgba_unmultiplied(47, 129, 247, 36),
            green: Color32::from_rgb(63, 185, 80),
            green_soft: Color32::from_rgba_unmultiplied(63, 185, 80, 32),
            red: Color32::from_rgb(248, 81, 73),
            red_soft: Color32::from_rgba_unmultiplied(248, 81, 73, 34),
            amber: Color32::from_rgb(210, 153, 34),
            amber_soft: Color32::from_rgba_unmultiplied(210, 153, 34, 34),
            purple: Color32::from_rgb(163, 113, 247),
            purple_soft: Color32::from_rgba_unmultiplied(163, 113, 247, 34),
            cyan: Color32::from_rgb(57, 197, 207),
            cyan_soft: Color32::from_rgba_unmultiplied(57, 197, 207, 34),
        }
    }

    fn light() -> Self {
        Self {
            sidebar: Color32::from_rgb(244, 246, 250),
            canvas: Color32::from_rgb(248, 250, 252),
            surface: Color32::from_rgb(255, 255, 255),
            elevated: Color32::from_rgb(241, 245, 249),
            hover: Color32::from_rgb(233, 238, 246),
            border: Color32::from_rgb(213, 221, 232),
            border_subtle: Color32::from_rgb(229, 234, 242),
            text: Color32::from_rgb(15, 23, 42),
            text_secondary: Color32::from_rgb(51, 65, 85),
            muted: Color32::from_rgb(100, 116, 139),
            accent: Color32::from_rgb(37, 99, 235),
            accent_soft: Color32::from_rgba_unmultiplied(37, 99, 235, 24),
            green: Color32::from_rgb(21, 128, 61),
            green_soft: Color32::from_rgba_unmultiplied(22, 163, 74, 24),
            red: Color32::from_rgb(220, 38, 38),
            red_soft: Color32::from_rgba_unmultiplied(220, 38, 38, 24),
            amber: Color32::from_rgb(180, 83, 9),
            amber_soft: Color32::from_rgba_unmultiplied(217, 119, 6, 26),
            purple: Color32::from_rgb(126, 34, 206),
            purple_soft: Color32::from_rgba_unmultiplied(147, 51, 234, 24),
            cyan: Color32::from_rgb(14, 116, 144),
            cyan_soft: Color32::from_rgba_unmultiplied(8, 145, 178, 24),
        }
    }

    fn status_colors(&self, status: Status) -> (Color32, Color32) {
        match status {
            Status::Clean => (self.green, self.green_soft),
            Status::Dirty | Status::Diverged | Status::NotOnGithub => (self.red, self.red_soft),
            Status::Ahead | Status::Behind | Status::Untracked => (self.amber, self.amber_soft),
            Status::Stashed | Status::NoUpstream | Status::NoRemote => {
                (self.purple, self.purple_soft)
            }
            Status::NotCloned => (self.cyan, self.cyan_soft),
        }
    }
}

pub struct RepoMApp {
    rt: Option<tokio::runtime::Runtime>,
    tx: Sender<BgEvent>,
    rx: Receiver<BgEvent>,

    config: Config,
    config_path: Option<PathBuf>,
    /// Optional in-memory GitHub token entered during this session (never written to disk).
    session_token: String,

    reports: Vec<RepoReport>,
    summary: Summary,
    last_scan: Option<DateTime<Utc>>,
    github_enabled: bool,
    github_error: Option<String>,
    banner_dismissed: bool,

    // UI state
    dark_mode: bool,
    busy_label: Option<String>,
    scan_total: usize,
    toast: Option<(String, bool)>,
    search: String,
    filter: StatusFilter,
    sort_mode: SortMode,
    hide_archived: bool,
    hide_forks: bool,
    selected_key: Option<String>,
    pending_confirm: Option<PendingConfirm>,
    show_settings: bool,
    settings_draft: SettingsDraft,
    settings_error: Option<String>,
}

impl RepoMApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        configure_enterprise_fonts(&cc.egui_ctx);

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to build Tokio runtime");
        let (tx, rx) = mpsc::channel();
        let (config, config_path) = Config::load_or_default();
        let settings_draft = SettingsDraft::from_config(&config);

        let app = Self {
            rt: Some(rt),
            tx,
            rx,
            config,
            config_path,
            session_token: String::new(),
            reports: Vec::new(),
            summary: Summary::default(),
            last_scan: None,
            github_enabled: false,
            github_error: None,
            banner_dismissed: false,
            dark_mode: true,
            busy_label: None,
            scan_total: 0,
            toast: None,
            search: String::new(),
            filter: StatusFilter::All,
            sort_mode: SortMode::Urgency,
            hide_archived: false,
            hide_forks: false,
            selected_key: None,
            pending_confirm: None,
            show_settings: false,
            settings_draft,
            settings_error: None,
        };

        app.apply_theme(&cc.egui_ctx);
        app
    }

    fn spawn(&self, task: impl std::future::Future<Output = ()> + Send + 'static) {
        if let Some(rt) = &self.rt {
            rt.spawn(task);
        }
    }

    fn effective_token(&self) -> Option<String> {
        let mem = self.session_token.trim();
        if !mem.is_empty() {
            Some(mem.to_string())
        } else {
            self.config.read_github_token()
        }
    }

    fn is_github_configured(&self) -> bool {
        self.effective_token().is_some() || self.config.github_user.is_some()
    }

    fn palette(&self) -> Palette {
        if self.dark_mode {
            Palette::dark()
        } else {
            Palette::light()
        }
    }

    fn apply_theme(&self, ctx: &egui::Context) {
        let pal = self.palette();
        let theme = if self.dark_mode {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        };
        ctx.set_theme(theme);

        let mut style = (*ctx.style()).clone();
        style.spacing.item_spacing = Vec2::new(8.0, 6.0);
        style.spacing.button_padding = Vec2::new(11.0, 6.0);
        style.spacing.interact_size.y = 28.0;
        style.spacing.combo_width = 185.0;

        style.text_styles = [
            (TextStyle::Heading, FontId::new(18.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(13.5, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(12.5, FontFamily::Monospace)),
            (TextStyle::Button, FontId::new(12.5, FontFamily::Proportional)),
            (TextStyle::Small, FontId::new(11.5, FontFamily::Proportional)),
        ]
        .into();

        let mut visuals = if self.dark_mode {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        visuals.panel_fill = pal.canvas;
        visuals.window_fill = pal.surface;
        visuals.extreme_bg_color = if self.dark_mode {
            pal.elevated
        } else {
            pal.surface
        };
        visuals.faint_bg_color = pal.elevated;
        // Do NOT set override_text_color — doing so overrides TextEdit placeholder/hint color
        // and breaks contrast hierarchy between primary, muted, and hint text.
        visuals.override_text_color = None;

        visuals.widgets.noninteractive.bg_fill = pal.surface;
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, pal.border);
        visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, pal.text);

        visuals.widgets.inactive.bg_fill = pal.elevated;
        visuals.widgets.inactive.weak_bg_fill = pal.elevated;
        visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, pal.border);
        visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, pal.text);
        visuals.widgets.inactive.rounding = Rounding::same(6.0);

        visuals.widgets.hovered.bg_fill = pal.hover;
        visuals.widgets.hovered.weak_bg_fill = pal.hover;
        visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, pal.accent);
        visuals.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, pal.text);
        visuals.widgets.hovered.rounding = Rounding::same(6.0);

        // Note: egui uses `widgets.active.fg_stroke.color` for `RichText::strong()`!
        // Keeping this set to `pal.text` ensures bold labels stay crisp & dark in Light Mode.
        visuals.widgets.active.bg_fill = pal.accent_soft;
        visuals.widgets.active.weak_bg_fill = pal.accent_soft;
        visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, pal.accent);
        visuals.widgets.active.fg_stroke = Stroke::new(1.0_f32, pal.text);
        visuals.widgets.active.rounding = Rounding::same(6.0);

        visuals.selection.bg_fill = pal.accent_soft;
        visuals.selection.stroke = Stroke::new(1.0_f32, pal.accent);
        visuals.window_rounding = Rounding::same(10.0);
        visuals.window_stroke = Stroke::new(1.0_f32, pal.border);
        visuals.menu_rounding = Rounding::same(8.0);

        style.visuals = visuals;
        ctx.set_style_of(theme, style);
    }

    fn repo_key(r: &RepoReport) -> String {
        if let Some(ref p) = r.path {
            p.to_string_lossy().to_string()
        } else if let Some(ref gh) = r.github_full_name {
            format!("gh:{}", gh)
        } else {
            format!("name:{}", r.name)
        }
    }

    fn trigger_scan(&mut self, ctx: &egui::Context, do_fetch: bool) {
        if self.busy_label.is_some() {
            return;
        }
        self.banner_dismissed = false;
        self.busy_label = Some("Discovering local repositories...".to_string());

        let cfg = self.config.clone();
        let github_enabled = self.github_enabled;
        let token = self.effective_token();
        let tx = self.tx.clone();
        let ctx_clone = ctx.clone();

        self.spawn(async move {
            let res = run_scan_task(
                &cfg,
                github_enabled,
                token.as_deref(),
                do_fetch,
                Some((tx.clone(), ctx_clone.clone())),
            )
            .await;
            match res {
                Ok((reports, gh_err)) => {
                    let count = reports.len();
                    let toast = if do_fetch {
                        format!("Fetched and scanned {} repositories.", count)
                    } else {
                        format!("Scanned {} repositories.", count)
                    };
                    let _ = tx.send(BgEvent::ScanDone {
                        reports,
                        github_enabled,
                        github_error: gh_err,
                        toast,
                    });
                }
                Err(err) => {
                    let _ = tx.send(BgEvent::Error(format!("{:#}", err)));
                }
            }
            ctx_clone.request_repaint();
        });
    }

    fn trigger_fetch_one(&mut self, ctx: &egui::Context, repo: RepoReport) {
        if self.busy_label.is_some() {
            return;
        }
        let Some(path) = repo.path.clone() else {
            return;
        };
        self.busy_label = Some(format!("Fetching {}...", repo.name));
        let cfg = self.config.clone();
        let github_enabled = self.github_enabled;
        let token = self.effective_token();
        let tx = self.tx.clone();
        let ctx_clone = ctx.clone();

        self.spawn(async move {
            let outcomes = fetch_repos_parallel(&[path], 45).await;
            let (success, toast) = match outcomes.into_iter().next() {
                Some((_, Ok(()))) => (true, format!("Fetched remotes for {}.", repo.name)),
                Some((_, Err(e))) => (false, format!("Fetch failed for {}: {}", repo.name, e)),
                None => (false, "Fetch did not run.".to_string()),
            };
            match run_scan_task(&cfg, github_enabled, token.as_deref(), false, None).await {
                Ok((reports, gh_err)) => {
                    let _ = tx.send(BgEvent::ActionDone {
                        success,
                        toast,
                        reports,
                        github_enabled,
                        github_error: gh_err,
                    });
                }
                Err(err) => {
                    let _ = tx.send(BgEvent::Error(format!("{:#}", err)));
                }
            }
            ctx_clone.request_repaint();
        });
    }

    fn trigger_pull(&mut self, ctx: &egui::Context, targets: Vec<RepoReport>) {
        if self.busy_label.is_some() || targets.is_empty() {
            return;
        }
        self.busy_label = Some(if targets.len() == 1 {
            format!("Pulling {} (ff-only)...", targets[0].name)
        } else {
            format!("Pulling {} clean repositories...", targets.len())
        });

        let cfg = self.config.clone();
        let github_enabled = self.github_enabled;
        let token = self.effective_token();
        let tx = self.tx.clone();
        let ctx_clone = ctx.clone();

        self.spawn(async move {
            let outcomes = pull_clean_repos(&targets, true, 60).await;
            let pulled = outcomes.iter().filter(|o| o.applied && o.success).count();
            let failed = outcomes.iter().filter(|o| o.applied && !o.success).count();
            let skipped = outcomes.iter().filter(|o| !o.applied).count();
            let toast = if outcomes.len() == 1 {
                format!("{}: {}", outcomes[0].repo_name, outcomes[0].message)
            } else {
                format!(
                    "Pull finished: {} pulled, {} skipped, {} failed.",
                    pulled, skipped, failed
                )
            };
            match run_scan_task(&cfg, github_enabled, token.as_deref(), false, None).await {
                Ok((reports, gh_err)) => {
                    let _ = tx.send(BgEvent::ActionDone {
                        success: failed == 0,
                        toast,
                        reports,
                        github_enabled,
                        github_error: gh_err,
                    });
                }
                Err(err) => {
                    let _ = tx.send(BgEvent::Error(format!("{:#}", err)));
                }
            }
            ctx_clone.request_repaint();
        });
    }

    fn trigger_clone(&mut self, ctx: &egui::Context, targets: Vec<RepoReport>, use_ssh: bool) {
        if self.busy_label.is_some() || targets.is_empty() {
            return;
        }
        self.busy_label = Some(if targets.len() == 1 {
            format!("Cloning {}...", targets[0].name)
        } else {
            format!("Cloning {} missing repositories...", targets.len())
        });

        let cfg = self.config.clone();
        let github_enabled = self.github_enabled;
        let token = self.effective_token();
        let tx = self.tx.clone();
        let ctx_clone = ctx.clone();

        self.spawn(async move {
            let clone_res =
                clone_missing_repos(&targets, &cfg.root, &cfg.root, use_ssh, true, 120).await;
            let (success, toast) = match clone_res {
                Ok(outcomes) => {
                    let cloned = outcomes.iter().filter(|o| o.applied && o.success).count();
                    let failed = outcomes.iter().filter(|o| !o.success).count();
                    let msg = if outcomes.len() == 1 {
                        format!("{}: {}", outcomes[0].repo_name, outcomes[0].message)
                    } else {
                        format!("Clone finished: {} cloned, {} failed.", cloned, failed)
                    };
                    (failed == 0, msg)
                }
                Err(err) => (false, format!("Clone blocked: {:#}", err)),
            };
            match run_scan_task(&cfg, github_enabled, token.as_deref(), false, None).await {
                Ok((reports, gh_err)) => {
                    let _ = tx.send(BgEvent::ActionDone {
                        success,
                        toast,
                        reports,
                        github_enabled,
                        github_error: gh_err,
                    });
                }
                Err(err) => {
                    let _ = tx.send(BgEvent::Error(format!("{:#}", err)));
                }
            }
            ctx_clone.request_repaint();
        });
    }

    fn poll_events(&mut self) {
        let mut reports_changed = false;
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                BgEvent::ScanDiscovered(total) => {
                    self.scan_total = total;
                    self.reports.clear();
                    reports_changed = true;
                    self.busy_label =
                        Some(format!("Found {total} repositories. Checking local status..."));
                }
                BgEvent::ScanInspected(report) => {
                    self.reports.push(*report);
                    reports_changed = true;
                    self.busy_label = Some(format!(
                        "Checked {} of {} local repositories...",
                        self.reports.len(),
                        self.scan_total,
                    ));
                }
                BgEvent::ScanFetching => {
                    self.busy_label = Some(format!(
                        "Fetching remotes for {} repositories...",
                        self.scan_total
                    ));
                }
                BgEvent::ScanComparing => {
                    self.busy_label = Some("Comparing with GitHub...".to_string());
                }
                BgEvent::ScanDone {
                    reports,
                    github_enabled,
                    github_error,
                    toast,
                } => {
                    self.busy_label = None;
                    self.summary = Summary::from_reports(&reports);
                    self.reports = reports;
                    self.github_enabled = github_enabled;
                    self.github_error = github_error;
                    self.last_scan = Some(Utc::now());
                    self.toast = Some((toast, true));
                }
                BgEvent::ActionDone {
                    success,
                    toast,
                    reports,
                    github_enabled,
                    github_error,
                } => {
                    self.busy_label = None;
                    self.summary = Summary::from_reports(&reports);
                    self.reports = reports;
                    self.github_enabled = github_enabled;
                    self.github_error = github_error;
                    self.last_scan = Some(Utc::now());
                    self.toast = Some((toast, success));
                }
                BgEvent::Error(msg) => {
                    self.busy_label = None;
                    self.toast = Some((msg, false));
                }
            }
        }
        if reports_changed {
            self.summary = Summary::from_reports(&self.reports);
        }
    }

    fn needs_attention_count(&self) -> usize {
        self.reports
            .iter()
            .filter(|r| {
                r.has_status(Status::Dirty)
                    || r.has_status(Status::Untracked)
                    || r.has_status(Status::Ahead)
                    || r.has_status(Status::Diverged)
                    || r.has_status(Status::NotOnGithub)
                    || r.error.is_some()
            })
            .count()
    }

    fn filtered_reports(&self) -> Vec<RepoReport> {
        let q = self.search.trim().to_ascii_lowercase();
        let mut list: Vec<RepoReport> = self
            .reports
            .iter()
            .filter(|r| {
                if self.hide_archived && r.archived {
                    return false;
                }
                if self.hide_forks && r.fork {
                    return false;
                }
                match self.filter {
                    StatusFilter::All => {}
                    StatusFilter::OutOfSync => {
                        if r.is_clean() {
                            return false;
                        }
                    }
                    StatusFilter::NeedsAttention => {
                        let urgent = [
                            Status::Dirty,
                            Status::Untracked,
                            Status::Ahead,
                            Status::Diverged,
                            Status::NotOnGithub,
                        ];
                        if !urgent.iter().any(|s| r.has_status(*s)) && r.error.is_none() {
                            return false;
                        }
                    }
                    StatusFilter::Exact(st) => {
                        if !r.has_status(st) {
                            return false;
                        }
                    }
                }
                if !q.is_empty() {
                    let matches = r.name.to_ascii_lowercase().contains(&q)
                        || r.branch
                            .as_deref()
                            .unwrap_or("")
                            .to_ascii_lowercase()
                            .contains(&q)
                        || r.remote_url
                            .as_deref()
                            .unwrap_or("")
                            .to_ascii_lowercase()
                            .contains(&q)
                        || r.path
                            .as_ref()
                            .map(|p| p.to_string_lossy().to_ascii_lowercase().contains(&q))
                            .unwrap_or(false);
                    if !matches {
                        return false;
                    }
                }
                true
            })
            .cloned()
            .collect();

        match self.sort_mode {
            SortMode::Urgency => {}
            SortMode::Name => {
                list.sort_by(|a, b| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()));
            }
            SortMode::LastCommit => {
                list.sort_by(|a, b| b.last_commit.cmp(&a.last_commit));
            }
            SortMode::DirtyFiles => {
                list.sort_by(|a, b| {
                    (b.dirty_files + b.untracked_files)
                        .cmp(&(a.dirty_files + a.untracked_files))
                        .then_with(|| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()))
                });
            }
        }
        list
    }

    fn export_report(&mut self, is_json: bool) {
        let filtered = self.filtered_reports();
        let output = ScanOutput {
            machine: self.config.machine.name.clone(),
            root: self.config.root.display().to_string(),
            generated_at: self.last_scan.unwrap_or_else(Utc::now),
            summary: Summary::from_reports(&filtered),
            repos: filtered,
        };

        let (default_name, ext_name, ext) = if is_json {
            ("repom-report.json", "JSON", "json")
        } else {
            ("repom-report.md", "Markdown", "md")
        };

        if let Some(path) = rfd::FileDialog::new()
            .set_file_name(default_name)
            .add_filter(ext_name, &[ext])
            .save_file()
        {
            let content_res = if is_json {
                render_json(&output)
            } else {
                Ok(render_markdown(&output))
            };
            match content_res.and_then(|c| write_report_file(&path, &c)) {
                Ok(()) => {
                    self.toast = Some((format!("Exported report to {}", path.display()), true));
                }
                Err(err) => {
                    self.toast = Some((format!("Export failed: {:#}", err), false));
                }
            }
        }
    }

    fn render_sidebar(&mut self, ctx: &egui::Context) {
        let pal = self.palette();
        let is_busy = self.busy_label.is_some();
        let s = self.summary.clone();
        let attention = self.needs_attention_count();
        let gh_configured = self.is_github_configured();

        egui::SidePanel::left("nav_sidebar")
            .resizable(false)
            .exact_width(236.0)
            .show_separator_line(false)
            .frame(
                egui::Frame::none()
                    .fill(pal.sidebar)
                    .inner_margin(Margin::symmetric(14.0, 12.0))
                    .stroke(Stroke::new(1.0_f32, pal.border_subtle)),
            )
            .show(ctx, |ui| {
                // Brand header
                ui.horizontal(|ui| {
                    let (icon_rect, _) = ui.allocate_exact_size(Vec2::new(32.0, 32.0), Sense::hover());
                    paint_brand_badge(ui.painter(), icon_rect, pal.accent);
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new("RepoM").strong().size(15.5).color(pal.text));
                        ui.label(
                            RichText::new(&self.config.machine.name)
                                .size(11.0)
                                .color(pal.muted),
                        );
                    });
                });

                ui.add_space(10.0);

                // Workspace card
                egui::Frame::none()
                    .fill(pal.surface)
                    .rounding(Rounding::same(8.0))
                    .stroke(Stroke::new(1.0_f32, pal.border_subtle))
                    .inner_margin(Margin::symmetric(10.0, 8.0))
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.label(
                            RichText::new("WORKSPACE")
                                .size(10.0)
                                .strong()
                                .color(pal.muted),
                        );
                        let folder_name = self
                            .config
                            .root
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| self.config.root.display().to_string());
                        ui.label(
                            RichText::new(&folder_name)
                                .strong()
                                .size(13.5)
                                .color(pal.text),
                        )
                        .on_hover_text(self.config.root.display().to_string());
                        ui.label(
                            RichText::new(truncate_middle(
                                &self.config.root.display().to_string(),
                                28,
                            ))
                            .font(FontId::new(11.0, FontFamily::Monospace))
                            .color(pal.muted),
                        )
                        .on_hover_text(self.config.root.display().to_string());

                        ui.add_space(6.0);
                        let browse_btn = egui::Button::new(
                            RichText::new("Switch Folder...")
                                .size(12.0)
                                .color(pal.text_secondary),
                        )
                        .min_size(Vec2::new(ui.available_width(), 26.0));
                        if ui.add_enabled(!is_busy, browse_btn).clicked() {
                            if let Some(dir) = rfd::FileDialog::new()
                                .set_directory(&self.config.root)
                                .pick_folder()
                            {
                                self.config.root = dir;
                                self.settings_draft = SettingsDraft::from_config(&self.config);
                                // Persist chosen workspace so next launch opens it automatically
                                if let Some(save_path) = self
                                    .config_path
                                    .clone()
                                    .or_else(|| Config::default_config_path().ok())
                                {
                                    if self.config.save_to_file(&save_path).is_ok() {
                                        self.config_path = Some(save_path);
                                    }
                                }
                                self.selected_key = None;
                                self.trigger_scan(ctx, false);
                            }
                        }
                    });

                ui.add_space(12.0);

                // Scrollable navigation views + statuses (leaves space for pinned footer)
                let footer_reserve = 118.0;
                let nav_height = (ui.available_height() - footer_reserve).max(120.0);
                egui::ScrollArea::vertical()
                    .id_salt("sidebar_nav_scroll")
                    .max_height(nav_height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.label(RichText::new("VIEWS").size(10.5).strong().color(pal.muted));
                        ui.add_space(3.0);

                        let views = [
                            (StatusFilter::All, "All Repositories", s.total, pal.accent),
                            (
                                StatusFilter::OutOfSync,
                                "Out of Sync",
                                s.total.saturating_sub(s.clean),
                                pal.amber,
                            ),
                            (
                                StatusFilter::NeedsAttention,
                                "Needs Commit / Push",
                                attention,
                                pal.red,
                            ),
                            (
                                StatusFilter::Exact(Status::Clean),
                                "Clean & Synced",
                                s.clean,
                                pal.green,
                            ),
                        ];
                        for (flt, label, count, dot_color) in views {
                            if sidebar_nav_item(
                                ui,
                                label,
                                count,
                                dot_color,
                                !self.show_settings && self.filter == flt,
                                &pal,
                            ) {
                                self.filter = flt;
                                self.show_settings = false;
                            }
                        }

                        ui.add_space(10.0);
                        ui.label(
                            RichText::new("BY STATUS")
                                .size(10.5)
                                .strong()
                                .color(pal.muted),
                        );
                        ui.add_space(3.0);

                        let status_items = [
                            (Status::Dirty, "Dirty", s.dirty),
                            (Status::Untracked, "Untracked", s.untracked),
                            (Status::Ahead, "Ahead", s.ahead),
                            (Status::Behind, "Behind", s.behind),
                            (Status::Diverged, "Diverged", s.diverged),
                            (Status::Stashed, "Stashed", s.stashed),
                            (Status::NoUpstream, "No Upstream", s.no_upstream),
                            (Status::NoRemote, "No Remote", s.no_remote),
                            (Status::NotOnGithub, "Not on GitHub", s.not_on_github),
                            (Status::NotCloned, "Not Cloned", s.not_cloned),
                        ];

                        for (st, label, count) in status_items {
                            let (dot, _) = pal.status_colors(st);
                            let flt = StatusFilter::Exact(st);
                            if sidebar_nav_item(
                                ui,
                                label,
                                count,
                                dot,
                                !self.show_settings && self.filter == flt,
                                &pal,
                            ) {
                                self.filter = flt;
                                self.show_settings = false;
                            }
                        }
                    });

                // Pinned bottom controls: GitHub sync card + Settings / Theme
                ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
                    ui.horizontal(|ui| {
                        let half = (ui.available_width() - 8.0) / 2.0;
                        let settings_btn = egui::Button::new(
                            RichText::new("Settings")
                                .size(12.0)
                                .color(if self.show_settings {
                                    Color32::WHITE
                                } else {
                                    pal.text
                                }),
                        )
                        .fill(if self.show_settings {
                            pal.accent
                        } else {
                            pal.elevated
                        })
                        .stroke(Stroke::new(
                            1.0_f32,
                            if self.show_settings {
                                pal.accent
                            } else {
                                pal.border
                            },
                        ))
                        .min_size(Vec2::new(half, 28.0));
                        if ui.add(settings_btn).clicked() {
                            if self.show_settings {
                                self.show_settings = false;
                            } else {
                                self.settings_draft = SettingsDraft::from_config(&self.config);
                                self.settings_error = None;
                                self.show_settings = true;
                            }
                        }
                        let theme_label = if self.dark_mode { "Light Mode" } else { "Dark Mode" };
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new(theme_label).size(12.0).color(pal.text),
                                )
                                .min_size(Vec2::new(half, 28.0)),
                            )
                            .clicked()
                        {
                            self.dark_mode = !self.dark_mode;
                            self.apply_theme(ctx);
                        }
                    });

                    ui.add_space(8.0);

                    egui::Frame::none()
                        .fill(pal.surface)
                        .rounding(Rounding::same(8.0))
                        .stroke(Stroke::new(1.0_f32, pal.border_subtle))
                        .inner_margin(Margin::symmetric(10.0, 9.0))
                        .show(ui, |ui| {
                            // Explicitly reset to top_down inside the card (parent is bottom_up!)
                            ui.with_layout(Layout::top_down(Align::LEFT), |ui| {
                                ui.set_min_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    let dot_color =
                                        if self.github_enabled && self.github_error.is_none() {
                                            pal.green
                                        } else if gh_configured {
                                            pal.accent
                                        } else {
                                            pal.amber
                                        };
                                    paint_dot(ui, dot_color, 4.0);
                                    ui.label(
                                        RichText::new("GitHub Sync")
                                            .strong()
                                            .size(12.5)
                                            .color(pal.text),
                                    );
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        if toggle_switch(
                                            ui,
                                            &mut self.github_enabled,
                                            !is_busy,
                                            &pal,
                                        ) {
                                            self.trigger_scan(ctx, false);
                                        }
                                    });
                                });
                                ui.add_space(3.0);
                                let sub = if self.github_enabled && self.github_error.is_none() {
                                    "Remote comparison active"
                                } else if gh_configured {
                                    "Ready - toggle on to compare"
                                } else {
                                    "Set user or token in Settings"
                                };
                                ui.label(RichText::new(sub).size(11.0).color(pal.muted));
                            });
                        });
                });
            });
    }

    fn render_header_bar(&mut self, ctx: &egui::Context, visible_count: usize) {
        let pal = self.palette();
        let is_busy = self.busy_label.is_some();
        let pullable: Vec<RepoReport> = self
            .reports
            .iter()
            .filter(|r| r.is_clean_pullable())
            .cloned()
            .collect();
        let missing: Vec<RepoReport> = self
            .reports
            .iter()
            .filter(|r| r.has_status(Status::NotCloned))
            .cloned()
            .collect();

        egui::TopBottomPanel::top("content_header")
            .exact_height(56.0)
            .show_separator_line(false)
            .frame(
                egui::Frame::none()
                    .fill(pal.surface)
                    .inner_margin(Margin::symmetric(20.0, 10.0))
                    .stroke(Stroke::new(1.0_f32, pal.border_subtle)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new(self.filter.title())
                                .strong()
                                .size(16.0)
                                .color(pal.text),
                        );
                        ui.label(
                            RichText::new(format!(
                                "Showing {} of {} repositories",
                                visible_count, self.summary.total
                            ))
                            .size(11.0)
                            .color(pal.muted),
                        );
                    });

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add_enabled(
                                !is_busy,
                                egui::Button::new(RichText::new("Export MD").size(12.0))
                                    .min_size(Vec2::new(0.0, 30.0)),
                            )
                            .clicked()
                        {
                            self.export_report(false);
                        }

                        if ui
                            .add_enabled(
                                !is_busy,
                                egui::Button::new(RichText::new("Export JSON").size(12.0))
                                    .min_size(Vec2::new(0.0, 30.0)),
                            )
                            .clicked()
                        {
                            self.export_report(true);
                        }

                        if !missing.is_empty()
                            && ui
                                .add_enabled(
                                    !is_busy,
                                    egui::Button::new(
                                        RichText::new(format!("Clone Missing ({})", missing.len()))
                                            .size(12.0),
                                    )
                                    .min_size(Vec2::new(0.0, 30.0)),
                                )
                                .clicked()
                        {
                            self.pending_confirm = Some(PendingConfirm::CloneAllMissing {
                                repos: missing,
                                use_ssh: false,
                            });
                        }

                        let has_pullable = !pullable.is_empty();
                        let pull_btn = egui::Button::new(
                            RichText::new(format!("Pull Clean ({})", pullable.len()))
                                .size(12.0)
                                .strong()
                                .color(if has_pullable {
                                    Color32::WHITE
                                } else {
                                    pal.muted
                                }),
                        )
                        .fill(if has_pullable { pal.accent } else { pal.elevated })
                        .stroke(Stroke::new(
                            1.0_f32,
                            if has_pullable { pal.accent } else { pal.border },
                        ))
                        .min_size(Vec2::new(0.0, 30.0));
                        if ui
                            .add_enabled(!is_busy && has_pullable, pull_btn)
                            .clicked()
                        {
                            self.pending_confirm =
                                Some(PendingConfirm::PullAllClean { repos: pullable });
                        }

                        if ui
                            .add_enabled(
                                !is_busy,
                                egui::Button::new(RichText::new("Fetch All").size(12.0))
                                    .min_size(Vec2::new(0.0, 30.0)),
                            )
                            .clicked()
                        {
                            self.trigger_scan(ctx, true);
                        }

                        if ui
                            .add_enabled(
                                !is_busy,
                                egui::Button::new(
                                    RichText::new(if self.last_scan.is_some() { "Rescan" } else { "Scan" })
                                        .size(12.0),
                                )
                                .min_size(Vec2::new(0.0, 30.0)),
                            )
                            .clicked()
                        {
                            self.trigger_scan(ctx, false);
                        }
                    });
                });
            });
    }

    fn render_bottom_bar(&self, ctx: &egui::Context) {
        let pal = self.palette();
        egui::TopBottomPanel::bottom("statusbar")
            .exact_height(30.0)
            .show_separator_line(false)
            .frame(
                egui::Frame::none()
                    .fill(pal.sidebar)
                    .inner_margin(Margin::symmetric(16.0, 5.0))
                    .stroke(Stroke::new(1.0_f32, pal.border_subtle)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    if let Some(ref label) = self.busy_label {
                        ui.add(egui::Spinner::new().size(14.0).color(pal.accent));
                        ui.label(RichText::new(label).color(pal.accent).size(11.5).strong());
                    } else if let Some((ref msg, ok)) = self.toast {
                        let color = if ok { pal.green } else { pal.red };
                        paint_dot(ui, color, 3.5);
                        ui.label(RichText::new(msg).color(pal.text_secondary).size(11.5));
                    } else {
                        paint_dot(ui, pal.green, 3.5);
                        ui.label(RichText::new("Ready").color(pal.muted).size(11.5));
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if let Some(ts) = self.last_scan {
                            let local_ts: DateTime<Local> = ts.with_timezone(&Local);
                            ui.label(
                                RichText::new(format!(
                                    "Last scan: {}",
                                    local_ts.format("%H:%M:%S")
                                ))
                                .color(pal.muted)
                                .size(11.0),
                            );
                            ui.label(RichText::new("|").color(pal.border).size(11.0));
                        }
                        ui.label(
                            RichText::new(format_summary_line(&self.summary))
                                .color(pal.muted)
                                .size(11.0),
                        );
                    });
                });
            });
    }

    fn render_kpi_strip(&mut self, ui: &mut egui::Ui) {
        let pal = self.palette();
        let s = self.summary.clone();
        let attention = self.needs_attention_count();
        let pullable = self
            .reports
            .iter()
            .filter(|r| r.is_clean_pullable())
            .count();
        let clean_pct = if s.total > 0 {
            (s.clean * 100) / s.total
        } else {
            0
        };
        let remote_gaps = s.no_remote + s.not_on_github + s.not_cloned;

        ui.columns(4, |cols| {
            // Card 1: Sync Health (with embedded segmented progress bar)
            {
                let active = self.filter == StatusFilter::Exact(Status::Clean);
                let resp = kpi_card_frame(&pal, active)
                    .show(&mut cols[0], |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.set_min_height(78.0);
                        ui.horizontal(|ui| {
                            paint_dot(ui, pal.green, 3.5);
                            ui.label(
                                RichText::new("SYNC HEALTH")
                                    .size(10.0)
                                    .strong()
                                    .color(pal.muted),
                            );
                        });
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("{}%", clean_pct))
                                    .size(24.0)
                                    .strong()
                                    .color(pal.green),
                            );
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new(format!("{} / {} clean", s.clean, s.total))
                                    .size(11.5)
                                    .color(pal.muted),
                            );
                        });
                        ui.add_space(6.0);
                        paint_health_bar(ui, &s, &pal);
                    })
                    .response
                    .interact(Sense::click());
                if resp.clicked() {
                    self.filter = if active {
                        StatusFilter::All
                    } else {
                        StatusFilter::Exact(Status::Clean)
                    };
                }
            }

            // Card 2: Needs Attention
            {
                let active = self.filter == StatusFilter::NeedsAttention;
                let accent = if attention > 0 { pal.red } else { pal.green };
                let resp = kpi_card_frame(&pal, active)
                    .show(&mut cols[1], |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.set_min_height(78.0);
                        ui.horizontal(|ui| {
                            paint_dot(ui, accent, 3.5);
                            ui.label(
                                RichText::new("NEEDS COMMIT / PUSH")
                                    .size(10.0)
                                    .strong()
                                    .color(pal.muted),
                            );
                        });
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(attention.to_string())
                                .size(24.0)
                                .strong()
                                .color(accent),
                        );
                        ui.add_space(2.0);
                        ui.label(
                            RichText::new(format!(
                                "{} dirty  |  {} ahead  |  {} diverged",
                                s.dirty, s.ahead, s.diverged
                            ))
                            .size(11.0)
                            .color(pal.muted),
                        );
                    })
                    .response
                    .interact(Sense::click());
                if resp.clicked() {
                    self.filter = if active {
                        StatusFilter::All
                    } else {
                        StatusFilter::NeedsAttention
                    };
                }
            }

            // Card 3: Behind Remote
            {
                let active = self.filter == StatusFilter::Exact(Status::Behind);
                let accent = if s.behind > 0 { pal.amber } else { pal.text_secondary };
                let resp = kpi_card_frame(&pal, active)
                    .show(&mut cols[2], |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.set_min_height(78.0);
                        ui.horizontal(|ui| {
                            paint_dot(ui, pal.amber, 3.5);
                            ui.label(
                                RichText::new("BEHIND REMOTE")
                                    .size(10.0)
                                    .strong()
                                    .color(pal.muted),
                            );
                        });
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(s.behind.to_string())
                                .size(24.0)
                                .strong()
                                .color(accent),
                        );
                        ui.add_space(2.0);
                        ui.label(
                            RichText::new(format!(
                                "{} clean & fast-forward pullable",
                                pullable
                            ))
                            .size(11.0)
                            .color(pal.muted),
                        );
                    })
                    .response
                    .interact(Sense::click());
                if resp.clicked() {
                    self.filter = if active {
                        StatusFilter::All
                    } else {
                        StatusFilter::Exact(Status::Behind)
                    };
                }
            }

            // Card 4: Remote Coverage
            {
                let active = self.filter == StatusFilter::Exact(Status::NoRemote);
                let accent = if remote_gaps > 0 { pal.purple } else { pal.cyan };
                let resp = kpi_card_frame(&pal, active)
                    .show(&mut cols[3], |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.set_min_height(78.0);
                        ui.horizontal(|ui| {
                            paint_dot(ui, accent, 3.5);
                            ui.label(
                                RichText::new("REMOTE COVERAGE")
                                    .size(10.0)
                                    .strong()
                                    .color(pal.muted),
                            );
                        });
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(remote_gaps.to_string())
                                .size(24.0)
                                .strong()
                                .color(accent),
                        );
                        ui.add_space(2.0);
                        ui.label(
                            RichText::new(format!(
                                "{} no remote  |  {} not cloned",
                                s.no_remote, s.not_cloned
                            ))
                            .size(11.0)
                            .color(pal.muted),
                        );
                    })
                    .response
                    .interact(Sense::click());
                if resp.clicked() {
                    self.filter = if active {
                        StatusFilter::All
                    } else {
                        StatusFilter::Exact(Status::NoRemote)
                    };
                }
            }
        });
    }

    fn render_table_card(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, rows: &[RepoReport]) {
        let pal = self.palette();
        let is_busy = self.busy_label.is_some();

        let mut action_fetch: Option<RepoReport> = None;
        let mut action_pull: Option<RepoReport> = None;
        let mut action_clone: Option<RepoReport> = None;
        let mut action_vscode: Option<PathBuf> = None;
        let mut newly_selected: Option<String> = None;

        egui::Frame::none()
            .fill(pal.surface)
            .rounding(Rounding::same(10.0))
            .stroke(Stroke::new(1.0_f32, pal.border))
            .inner_margin(Margin::same(0.0))
            .show(ui, |ui| {
                // Integrated table toolbar
                egui::Frame::none()
                    .fill(pal.surface)
                    .inner_margin(Margin::symmetric(14.0, 10.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.search)
                                    .hint_text("Search repositories, branches, paths, or remotes...")
                                    .desired_width(320.0),
                            );
                            if !self.search.is_empty() && ui.small_button("Clear").clicked() {
                                self.search.clear();
                            }

                            ui.add_space(8.0);
                            filter_chip(ui, "Hide Archived", &mut self.hide_archived, &pal);
                            filter_chip(ui, "Hide Forks", &mut self.hide_forks, &pal);

                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                egui::ComboBox::from_id_salt("sort_mode")
                                    .selected_text(format!("Sort: {}", self.sort_mode.label()))
                                    .show_ui(ui, |ui| {
                                        for mode in [
                                            SortMode::Urgency,
                                            SortMode::Name,
                                            SortMode::LastCommit,
                                            SortMode::DirtyFiles,
                                        ] {
                                            ui.selectable_value(
                                                &mut self.sort_mode,
                                                mode,
                                                mode.label(),
                                            );
                                        }
                                    });
                            });
                        });
                    });

                // Subtle divider between toolbar and table header
                let div_rect =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover()).0;
                ui.painter().rect_filled(div_rect, Rounding::ZERO, pal.border_subtle);

                if rows.is_empty() {
                    let avail = ui.available_height().max(160.0);
                    ui.allocate_ui_with_layout(
                        Vec2::new(ui.available_width(), avail),
                        Layout::centered_and_justified(egui::Direction::TopDown),
                        |ui| {
                            ui.vertical_centered(|ui| {
                                ui.add_space(avail * 0.35);
                                ui.label(
                                    RichText::new(if let Some(label) = self.busy_label.as_deref() {
                                        label
                                    } else if self.last_scan.is_none() {
                                        "Ready to scan your workspace"
                                    } else if self.reports.is_empty() {
                                        "No Git repositories found"
                                    } else {
                                        "No repositories match the current view"
                                    })
                                    .size(14.0)
                                    .strong()
                                    .color(pal.text_secondary),
                                );
                                if self.busy_label.is_none() {
                                    ui.add_space(4.0);
                                    let hint = if self.last_scan.is_none() {
                                        "Select Scan to find local Git repositories."
                                    } else if self.reports.is_empty() {
                                        "Check the workspace folder, scan depth, and exclusions in Settings."
                                    } else {
                                        "Try selecting 'All Repositories' in the sidebar or clearing your search."
                                    };
                                    ui.label(RichText::new(hint).size(12.0).color(pal.muted));
                                }
                                if self.filter != StatusFilter::All || !self.search.is_empty() {
                                    ui.add_space(10.0);
                                    if ui.button("Reset Filters").clicked() {
                                        self.filter = StatusFilter::All;
                                        self.search.clear();
                                    }
                                }
                            });
                        },
                    );
                    return;
                }

                egui::Frame::none()
                    .inner_margin(Margin::symmetric(12.0, 6.0))
                    .show(ui, |ui| {
                        TableBuilder::new(ui)
                            .striped(true)
                            .resizable(true)
                            .cell_layout(Layout::left_to_right(Align::Center))
                            .column(Column::initial(220.0).at_least(150.0)) // Repository
                            .column(Column::initial(115.0).at_least(80.0)) // Branch
                            .column(Column::remainder().at_least(180.0)) // Status
                            .column(Column::initial(85.0)) // Sync
                            .column(Column::initial(125.0)) // Changes
                            .column(Column::initial(55.0)) // Stash
                            .column(Column::initial(95.0)) // Last Commit
                            .column(Column::initial(195.0)) // Actions
                            .sense(Sense::click())
                            .header(28.0, |mut header| {
                                for title in [
                                    "REPOSITORY",
                                    "BRANCH",
                                    "STATUS",
                                    "SYNC",
                                    "WORKING TREE",
                                    "STASH",
                                    "LAST COMMIT",
                                    "ACTIONS",
                                ] {
                                    header.col(|ui| {
                                        ui.label(
                                            RichText::new(title)
                                                .size(10.0)
                                                .strong()
                                                .color(pal.muted),
                                        );
                                    });
                                }
                            })
                            .body(|body| {
                                body.rows(36.0, rows.len(), |mut row| {
                                    let r = &rows[row.index()];
                                    let key = Self::repo_key(r);
                                    let is_selected =
                                        self.selected_key.as_deref() == Some(key.as_str());
                                    let is_not_cloned = r.has_status(Status::NotCloned);

                                    row.set_selected(is_selected);

                                    // Repository name + tags
                                    row.col(|ui| {
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                RichText::new(&r.name)
                                                    .strong()
                                                    .size(13.0)
                                                    .color(pal.text),
                                            );
                                            if r.archived {
                                                draw_tag(ui, "archived", pal.muted, pal.elevated);
                                            }
                                            if r.fork {
                                                draw_tag(ui, "fork", pal.muted, pal.elevated);
                                            }
                                        });
                                    });

                                    // Branch chip
                                    row.col(|ui| {
                                        if is_not_cloned {
                                            ui.label(RichText::new("-").color(pal.muted));
                                        } else {
                                            let b = r.branch.as_deref().unwrap_or("-");
                                            let fg = if r.detached_head {
                                                pal.amber
                                            } else {
                                                pal.text_secondary
                                            };
                                            egui::Frame::none()
                                                .fill(pal.elevated)
                                                .rounding(Rounding::same(5.0))
                                                .stroke(Stroke::new(1.0_f32, pal.border_subtle))
                                                .inner_margin(Margin::symmetric(6.0, 2.0))
                                                .show(ui, |ui| {
                                                    ui.label(
                                                        RichText::new(b)
                                                            .font(FontId::new(
                                                                11.5,
                                                                FontFamily::Monospace,
                                                            ))
                                                            .color(fg),
                                                    );
                                                });
                                        }
                                    });

                                    // Status badges
                                    row.col(|ui| {
                                        if let Some(ref err) = r.error {
                                            draw_status_badge(ui, "ERROR", pal.red, pal.red_soft)
                                                .on_hover_text(err);
                                        } else {
                                            for &st in &r.statuses {
                                                let (fg, bg) = pal.status_colors(st);
                                                draw_status_badge(ui, st.as_str(), fg, bg);
                                            }
                                        }
                                    });

                                    // Ahead / Behind (pure ASCII +N / -M — zero missing glyphs)
                                    row.col(|ui| {
                                        match (r.ahead, r.behind) {
                                            (Some(a), Some(b)) => {
                                                let a_col = if a > 0 { pal.amber } else { pal.muted };
                                                let b_col = if b > 0 { pal.cyan } else { pal.muted };
                                                ui.label(
                                                    RichText::new(format!("+{}", a))
                                                        .font(FontId::new(
                                                            11.5,
                                                            FontFamily::Monospace,
                                                        ))
                                                        .color(a_col),
                                                );
                                                ui.label(
                                                    RichText::new(format!("-{}", b))
                                                        .font(FontId::new(
                                                            11.5,
                                                            FontFamily::Monospace,
                                                        ))
                                                        .color(b_col),
                                                );
                                            }
                                            _ => {
                                                ui.label(RichText::new("-").color(pal.muted));
                                            }
                                        }
                                    });

                                    // Working tree changes
                                    row.col(|ui| {
                                        if is_not_cloned {
                                            ui.label(RichText::new("-").color(pal.muted));
                                        } else if r.dirty_files == 0 && r.untracked_files == 0 {
                                            ui.label(
                                                RichText::new("Clean")
                                                    .size(11.5)
                                                    .color(pal.muted),
                                            );
                                        } else {
                                            let text = format!(
                                                "{} mod, {} new",
                                                r.dirty_files, r.untracked_files
                                            );
                                            ui.label(
                                                RichText::new(text)
                                                    .font(FontId::new(11.5, FontFamily::Monospace))
                                                    .color(pal.amber),
                                            );
                                        }
                                    });

                                    // Stash count
                                    row.col(|ui| {
                                        if is_not_cloned || r.stashes == 0 {
                                            ui.label(RichText::new("-").color(pal.muted));
                                        } else {
                                            draw_tag(
                                                ui,
                                                &r.stashes.to_string(),
                                                pal.purple,
                                                pal.purple_soft,
                                            );
                                        }
                                    });

                                    // Last commit date
                                    row.col(|ui| {
                                        let d = r
                                            .last_commit
                                            .map(|dt| dt.format("%Y-%m-%d").to_string())
                                            .unwrap_or_else(|| "-".to_string());
                                        ui.label(
                                            RichText::new(d)
                                                .font(FontId::new(11.5, FontFamily::Monospace))
                                                .color(pal.muted),
                                        );
                                    });

                                    // Inline quick actions
                                    row.col(|ui| {
                                        if !is_not_cloned {
                                            if let Some(ref p) = r.path {
                                                if ui
                                                    .add(
                                                        egui::Button::new(
                                                            RichText::new("VS Code").size(11.5),
                                                        )
                                                        .min_size(Vec2::new(62.0, 24.0)),
                                                    )
                                                    .on_hover_text("Open repository in VS Code")
                                                    .clicked()
                                                {
                                                    action_vscode = Some(p.clone());
                                                }
                                            }
                                            if ui
                                                .add_enabled(
                                                    !is_busy,
                                                    egui::Button::new(
                                                        RichText::new("Fetch").size(11.5),
                                                    )
                                                    .min_size(Vec2::new(48.0, 24.0)),
                                                )
                                                .clicked()
                                            {
                                                action_fetch = Some(r.clone());
                                            }
                                            if r.is_clean_pullable()
                                                && ui
                                                    .add_enabled(
                                                        !is_busy,
                                                        egui::Button::new(
                                                            RichText::new("Pull")
                                                                .size(11.5)
                                                                .strong()
                                                                .color(Color32::WHITE),
                                                        )
                                                        .fill(pal.accent)
                                                        .stroke(Stroke::new(1.0_f32, pal.accent))
                                                        .min_size(Vec2::new(44.0, 24.0)),
                                                    )
                                                    .clicked()
                                            {
                                                action_pull = Some(r.clone());
                                            }
                                        } else if ui
                                            .add_enabled(
                                                !is_busy,
                                                egui::Button::new(
                                                    RichText::new("Clone")
                                                        .size(11.5)
                                                        .strong()
                                                        .color(Color32::WHITE),
                                                )
                                                .fill(pal.accent)
                                                .stroke(Stroke::new(1.0_f32, pal.accent))
                                                .min_size(Vec2::new(56.0, 24.0)),
                                            )
                                            .clicked()
                                        {
                                            action_clone = Some(r.clone());
                                        }
                                    });

                                    if row.response().clicked() {
                                        newly_selected = Some(key);
                                    }
                                });
                            });
                    });
            });

        if let Some(k) = newly_selected {
            self.selected_key = Some(k);
        }
        if let Some(p) = action_vscode {
            match open_local_target(&p, "vscode") {
                Ok(m) => self.toast = Some((m, true)),
                Err(e) => self.toast = Some((e, false)),
            }
        }
        if let Some(r) = action_fetch {
            self.trigger_fetch_one(ctx, r);
        }
        if let Some(r) = action_pull {
            self.pending_confirm = Some(PendingConfirm::PullOne { repo: r });
        }
        if let Some(r) = action_clone {
            self.pending_confirm = Some(PendingConfirm::CloneOne {
                repo: r,
                use_ssh: false,
            });
        }
    }

    fn render_inspector(&mut self, ctx: &egui::Context) {
        let Some(ref key) = self.selected_key else {
            return;
        };
        let Some(repo) = self
            .reports
            .iter()
            .find(|r| Self::repo_key(r) == *key)
            .cloned()
        else {
            self.selected_key = None;
            return;
        };

        let pal = self.palette();
        let is_busy = self.busy_label.is_some();
        let is_not_cloned = repo.has_status(Status::NotCloned);

        egui::SidePanel::right("inspector_panel")
            .resizable(true)
            .default_width(370.0)
            .min_width(310.0)
            .show_separator_line(false)
            .frame(
                egui::Frame::none()
                    .fill(pal.surface)
                    .inner_margin(Margin::same(18.0))
                    .stroke(Stroke::new(1.0_f32, pal.border_subtle)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&repo.name).strong().size(17.0));
                        if let Some(ref gh) = repo.github_full_name {
                            ui.label(
                                RichText::new(gh)
                                    .font(FontId::new(11.5, FontFamily::Monospace))
                                    .color(pal.muted),
                            );
                        }
                    });
                    ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
                        if ui
                            .add(egui::Button::new("Close").min_size(Vec2::new(0.0, 24.0)))
                            .clicked()
                        {
                            self.selected_key = None;
                        }
                    });
                });

                ui.add_space(10.0);
                ui.horizontal_wrapped(|ui| {
                    for &st in &repo.statuses {
                        let (fg, bg) = pal.status_colors(st);
                        draw_status_badge(ui, st.as_str(), fg, bg);
                    }
                    if repo.archived {
                        draw_tag(ui, "ARCHIVED", pal.muted, pal.elevated);
                    }
                    if repo.fork {
                        draw_tag(ui, "FORK", pal.muted, pal.elevated);
                    }
                    if repo.is_bare {
                        draw_tag(ui, "BARE", pal.muted, pal.elevated);
                    }
                    if repo.is_worktree {
                        draw_tag(ui, "WORKTREE", pal.muted, pal.elevated);
                    }
                });

                ui.add_space(12.0);
                ui.label(RichText::new("ACTIONS").size(10.0).strong().color(pal.muted));
                ui.add_space(6.0);

                ui.horizontal_wrapped(|ui| {
                    if !is_not_cloned {
                        if ui
                            .add_enabled(
                                !is_busy,
                                egui::Button::new("Fetch Remote").min_size(Vec2::new(0.0, 28.0)),
                            )
                            .clicked()
                        {
                            self.trigger_fetch_one(ctx, repo.clone());
                        }
                        let can_pull = !is_busy && repo.is_clean_pullable();
                        if ui
                            .add_enabled(
                                can_pull,
                                egui::Button::new(
                                    RichText::new("Pull (ff-only)").color(if can_pull {
                                        Color32::WHITE
                                    } else {
                                        pal.muted
                                    }),
                                )
                                .fill(if can_pull { pal.accent } else { pal.elevated })
                                .min_size(Vec2::new(0.0, 28.0)),
                            )
                            .clicked()
                        {
                            self.pending_confirm = Some(PendingConfirm::PullOne {
                                repo: repo.clone(),
                            });
                        }
                        if let Some(ref p) = repo.path {
                            if ui
                                .add(egui::Button::new("Explorer").min_size(Vec2::new(0.0, 28.0)))
                                .clicked()
                            {
                                match open_local_target(p, "explorer") {
                                    Ok(m) => self.toast = Some((m, true)),
                                    Err(e) => self.toast = Some((e, false)),
                                }
                            }
                            if ui
                                .add(
                                    egui::Button::new("Open with VS Code")
                                        .min_size(Vec2::new(0.0, 28.0)),
                                )
                                .clicked()
                            {
                                match open_local_target(p, "vscode") {
                                    Ok(m) => self.toast = Some((m, true)),
                                    Err(e) => self.toast = Some((e, false)),
                                }
                            }
                            if ui
                                .add(egui::Button::new("Terminal").min_size(Vec2::new(0.0, 28.0)))
                                .clicked()
                            {
                                match open_local_target(p, "terminal") {
                                    Ok(m) => self.toast = Some((m, true)),
                                    Err(e) => self.toast = Some((e, false)),
                                }
                            }
                        }
                    } else if ui
                        .add_enabled(
                            !is_busy,
                            egui::Button::new(
                                RichText::new("Clone Repository").color(Color32::WHITE),
                            )
                            .fill(pal.accent)
                            .min_size(Vec2::new(0.0, 28.0)),
                        )
                        .clicked()
                    {
                        self.pending_confirm = Some(PendingConfirm::CloneOne {
                            repo: repo.clone(),
                            use_ssh: false,
                        });
                    }

                    if let Some(gh_url) = github_web_url(&repo) {
                        if ui
                            .add(egui::Button::new("Open GitHub").min_size(Vec2::new(0.0, 28.0)))
                            .clicked()
                        {
                            match open_github_url(&gh_url) {
                                Ok(m) => self.toast = Some((m, true)),
                                Err(e) => self.toast = Some((e, false)),
                            }
                        }
                    }
                });

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(8.0);

                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.label(
                        RichText::new("REPOSITORY METADATA")
                            .size(10.0)
                            .strong()
                            .color(pal.muted),
                    );
                    ui.add_space(6.0);

                    egui::Frame::none()
                        .fill(pal.elevated)
                        .rounding(Rounding::same(8.0))
                        .stroke(Stroke::new(1.0_f32, pal.border_subtle))
                        .inner_margin(Margin::same(12.0))
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            egui::Grid::new("inspector_kv")
                                .num_columns(2)
                                .spacing([14.0, 8.0])
                                .show(ui, |ui| {
                                    kv_row(
                                        ui,
                                        "Branch",
                                        repo.branch.as_deref().unwrap_or("-"),
                                        pal.muted,
                                    );
                                    let ab = match (repo.ahead, repo.behind) {
                                        (Some(a), Some(b)) => format!("+{} ahead, -{} behind", a, b),
                                        _ => "No tracking upstream".to_string(),
                                    };
                                    kv_row(ui, "Sync", &ab, pal.muted);
                                    let ch = if is_not_cloned {
                                        "-".to_string()
                                    } else {
                                        format!(
                                            "{} modified, {} untracked",
                                            repo.dirty_files, repo.untracked_files
                                        )
                                    };
                                    kv_row(ui, "Changes", &ch, pal.muted);
                                    kv_row(
                                        ui,
                                        "Stashes",
                                        &if is_not_cloned {
                                            "-".to_string()
                                        } else {
                                            repo.stashes.to_string()
                                        },
                                        pal.muted,
                                    );
                                    let lc = repo
                                        .last_commit
                                        .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
                                        .unwrap_or_else(|| "-".to_string());
                                    kv_row(ui, "Last Commit", &lc, pal.muted);
                                });
                        });

                    if let Some(ref p) = repo.path {
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("LOCAL PATH")
                                    .size(10.0)
                                    .strong()
                                    .color(pal.muted),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.small_button("Copy Path").clicked() {
                                    ui.output_mut(|o| o.copied_text = p.display().to_string());
                                    self.toast =
                                        Some(("Copied path to clipboard.".to_string(), true));
                                }
                            });
                        });
                        ui.add_space(4.0);
                        egui::Frame::none()
                            .fill(pal.elevated)
                            .rounding(Rounding::same(6.0))
                            .inner_margin(Margin::symmetric(10.0, 7.0))
                            .show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
                                ui.label(
                                    RichText::new(p.display().to_string())
                                        .font(FontId::new(11.5, FontFamily::Monospace))
                                        .color(pal.text_secondary),
                                );
                            });
                    }

                    if !repo.remotes.is_empty() {
                        ui.add_space(12.0);
                        ui.label(RichText::new("REMOTES").size(10.0).strong().color(pal.muted));
                        ui.add_space(4.0);
                        for rem in &repo.remotes {
                            egui::Frame::none()
                                .fill(pal.elevated)
                                .rounding(Rounding::same(6.0))
                                .inner_margin(Margin::symmetric(10.0, 6.0))
                                .show(ui, |ui| {
                                    ui.set_min_width(ui.available_width());
                                    ui.label(
                                        RichText::new(&rem.name).strong().size(11.0).color(pal.accent),
                                    );
                                    ui.label(
                                        RichText::new(&rem.url)
                                            .font(FontId::new(11.0, FontFamily::Monospace))
                                            .color(pal.text_secondary),
                                    );
                                });
                            ui.add_space(4.0);
                        }
                    } else if let Some(ref url) = repo.remote_url {
                        ui.add_space(12.0);
                        ui.label(
                            RichText::new("REMOTE URL")
                                .size(10.0)
                                .strong()
                                .color(pal.muted),
                        );
                        ui.label(
                            RichText::new(url)
                                .font(FontId::new(11.5, FontFamily::Monospace))
                                .color(pal.text_secondary),
                        );
                    }

                    if !repo.submodules.is_empty() {
                        ui.add_space(12.0);
                        ui.label(
                            RichText::new("SUBMODULES")
                                .size(10.0)
                                .strong()
                                .color(pal.muted),
                        );
                        for sm in &repo.submodules {
                            ui.label(
                                RichText::new(format!("- {}", sm))
                                    .font(FontId::new(11.5, FontFamily::Monospace)),
                            );
                        }
                    }

                    if let Some(ref err) = repo.error {
                        ui.add_space(12.0);
                        ui.label(RichText::new("ERROR").size(10.0).strong().color(pal.red));
                        ui.label(RichText::new(err).color(pal.red));
                    }
                });
            });
    }

    fn render_confirm_modal(&mut self, ctx: &egui::Context) {
        let Some(mut confirm) = self.pending_confirm.take() else {
            return;
        };
        let pal = self.palette();
        let mut keep_open = true;
        let mut confirmed = false;

        let title = match &confirm {
            PendingConfirm::PullOne { repo } => format!("Pull {} (ff-only)", repo.name),
            PendingConfirm::PullAllClean { repos } => {
                format!("Pull {} Clean Repositories", repos.len())
            }
            PendingConfirm::CloneOne { repo, .. } => format!("Clone {}", repo.name),
            PendingConfirm::CloneAllMissing { repos, .. } => {
                format!("Clone {} Missing Repositories", repos.len())
            }
        };

        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_min_width(400.0);
                match &mut confirm {
                    PendingConfirm::PullOne { repo } => {
                        ui.label(format!(
                            "Runs `git pull --ff-only` on '{}' (behind by {} commits). Dirty or diverged repositories are never modified.",
                            repo.name,
                            repo.behind.unwrap_or(0)
                        ));
                    }
                    PendingConfirm::PullAllClean { repos } => {
                        ui.label(
                            "Runs `git pull --ff-only` strictly on these clean behind repositories:",
                        );
                        ui.add_space(6.0);
                        egui::ScrollArea::vertical().max_height(150.0).show(ui, |ui| {
                            for r in repos.iter() {
                                ui.label(
                                    RichText::new(format!(
                                        "- {} (-{} behind)",
                                        r.name,
                                        r.behind.unwrap_or(0)
                                    ))
                                    .font(FontId::new(11.5, FontFamily::Monospace)),
                                );
                            }
                        });
                    }
                    PendingConfirm::CloneOne { repo, use_ssh } => {
                        ui.label(format!(
                            "Clones {} into {}",
                            repo.github_full_name.as_deref().unwrap_or(&repo.name),
                            self.config.root.display()
                        ));
                        ui.add_space(8.0);
                        filter_chip(ui, "Use SSH URL (git@github.com:...)", use_ssh, &pal);
                    }
                    PendingConfirm::CloneAllMissing { repos, use_ssh } => {
                        ui.label(format!(
                            "Clones {} missing GitHub repositories into {}:",
                            repos.len(),
                            self.config.root.display()
                        ));
                        ui.add_space(6.0);
                        filter_chip(ui, "Use SSH URLs (git@github.com:...)", use_ssh, &pal);
                        ui.add_space(6.0);
                        egui::ScrollArea::vertical().max_height(150.0).show(ui, |ui| {
                            for r in repos.iter() {
                                ui.label(
                                    RichText::new(format!("- {}", r.name))
                                        .font(FontId::new(11.5, FontFamily::Monospace)),
                                );
                            }
                        });
                    }
                }

                ui.add_space(14.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("Confirm").strong().color(Color32::WHITE),
                            )
                            .fill(pal.accent)
                            .stroke(Stroke::new(1.0_f32, pal.accent)),
                        )
                        .clicked()
                    {
                        confirmed = true;
                        keep_open = false;
                    }
                    if ui.button("Cancel").clicked() {
                        keep_open = false;
                    }
                });
            });

        if confirmed {
            match confirm {
                PendingConfirm::PullOne { repo } => self.trigger_pull(ctx, vec![repo]),
                PendingConfirm::PullAllClean { repos } => self.trigger_pull(ctx, repos),
                PendingConfirm::CloneOne { repo, use_ssh } => {
                    self.trigger_clone(ctx, vec![repo], use_ssh)
                }
                PendingConfirm::CloneAllMissing { repos, use_ssh } => {
                    self.trigger_clone(ctx, repos, use_ssh)
                }
            }
        } else if keep_open {
            self.pending_confirm = Some(confirm);
        }
    }

    fn render_settings_page(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        let pal = self.palette();
        let mut should_save = false;

        egui::ScrollArea::vertical()
            .id_salt("settings_page_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_max_width(760.0);

                // Section 1: Workspace & Scanning
                settings_section_card(
                    ui,
                    "Workspace & Scanning",
                    "Choose the root folder containing your local Git repositories and scan rules.",
                    &pal,
                    |ui| {
                        egui::Grid::new("settings_workspace_grid")
                            .num_columns(2)
                            .spacing([18.0, 10.0])
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new("Root Folder")
                                        .size(12.5)
                                        .color(pal.text_secondary),
                                );
                                ui.horizontal(|ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.settings_draft.root)
                                            .desired_width(380.0),
                                    );
                                    if ui.button("Browse...").clicked() {
                                        if let Some(dir) = rfd::FileDialog::new()
                                            .set_directory(&self.config.root)
                                            .pick_folder()
                                        {
                                            self.settings_draft.root = dir.display().to_string();
                                        }
                                    }
                                });
                                ui.end_row();

                                ui.label(
                                    RichText::new("Max Scan Depth")
                                        .size(12.5)
                                        .color(pal.text_secondary),
                                );
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.settings_draft.max_depth)
                                        .desired_width(90.0),
                                );
                                ui.end_row();

                                ui.label(
                                    RichText::new("Exclude Folders (CSV)")
                                        .size(12.5)
                                        .color(pal.text_secondary),
                                );
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.settings_draft.exclude)
                                        .hint_text("node_modules, target, .next, 99_Archive")
                                        .desired_width(460.0),
                                );
                                ui.end_row();
                            });
                    },
                );

                ui.add_space(12.0);

                // Section 2: GitHub Integration
                settings_section_card(
                    ui,
                    "GitHub Integration",
                    "Compare local repositories against your GitHub account and organizations. Tokens are never written to disk.",
                    &pal,
                    |ui| {
                        egui::Grid::new("settings_github_grid")
                            .num_columns(2)
                            .spacing([18.0, 10.0])
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new("GitHub Username")
                                        .size(12.5)
                                        .color(pal.text_secondary),
                                );
                                ui.add(
                                    egui::TextEdit::singleline(
                                        &mut self.settings_draft.github_user,
                                    )
                                    .hint_text("your-username")
                                    .desired_width(460.0),
                                );
                                ui.end_row();

                                ui.label(
                                    RichText::new("GitHub Orgs (CSV)")
                                        .size(12.5)
                                        .color(pal.text_secondary),
                                );
                                ui.add(
                                    egui::TextEdit::singleline(
                                        &mut self.settings_draft.github_orgs,
                                    )
                                    .hint_text("veltrion, other-org")
                                    .desired_width(460.0),
                                );
                                ui.end_row();

                                ui.label(
                                    RichText::new("Token Env Variable")
                                        .size(12.5)
                                        .color(pal.text_secondary),
                                );
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.settings_draft.token_env)
                                        .hint_text("GITHUB_TOKEN")
                                        .desired_width(460.0),
                                );
                                ui.end_row();

                                ui.label(
                                    RichText::new("Session Token (RAM)")
                                        .size(12.5)
                                        .color(pal.text_secondary),
                                );
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.session_token)
                                        .password(true)
                                        .hint_text(
                                            "Optional override for this session only - never saved to config.toml",
                                        )
                                        .desired_width(460.0),
                                );
                                ui.end_row();
                            });
                    },
                );

                ui.add_space(12.0);

                // Section 3: Machine & Privacy
                settings_section_card(
                    ui,
                    "Machine & Privacy",
                    "Identify this computer in exported reports and exclude private/work folders from scans and exports.",
                    &pal,
                    |ui| {
                        egui::Grid::new("settings_machine_grid")
                            .num_columns(2)
                            .spacing([18.0, 10.0])
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new("Machine Name")
                                        .size(12.5)
                                        .color(pal.text_secondary),
                                );
                                ui.add(
                                    egui::TextEdit::singleline(
                                        &mut self.settings_draft.machine_name,
                                    )
                                    .desired_width(460.0),
                                );
                                ui.end_row();

                                ui.label(
                                    RichText::new("Skip Folders (CSV)")
                                        .size(12.5)
                                        .color(pal.text_secondary),
                                );
                                ui.add(
                                    egui::TextEdit::singleline(
                                        &mut self.settings_draft.skip_folders,
                                    )
                                    .hint_text("e.g. 02_Rainco")
                                    .desired_width(460.0),
                                );
                                ui.end_row();
                            });
                    },
                );

                ui.add_space(12.0);

                let target_path = self
                    .config_path
                    .clone()
                    .or_else(|| Config::default_config_path().ok())
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "config.toml".to_string());
                ui.label(
                    RichText::new(format!("Configuration file: {}", target_path))
                        .font(FontId::new(11.5, FontFamily::Monospace))
                        .color(pal.muted),
                );

                if let Some(ref err) = self.settings_error {
                    ui.add_space(8.0);
                    egui::Frame::none()
                        .fill(pal.red_soft)
                        .rounding(Rounding::same(6.0))
                        .stroke(Stroke::new(1.0_f32, pal.red))
                        .inner_margin(Margin::symmetric(10.0, 7.0))
                        .show(ui, |ui| {
                            ui.label(RichText::new(err).color(pal.red).size(12.0));
                        });
                }

                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            self.busy_label.is_none(),
                            egui::Button::new(
                                RichText::new("Save & Rescan")
                                    .strong()
                                    .color(Color32::WHITE),
                            )
                            .fill(pal.accent)
                            .stroke(Stroke::new(1.0_f32, pal.accent))
                            .min_size(Vec2::new(130.0, 32.0)),
                        )
                        .clicked()
                    {
                        should_save = true;
                    }
                    if ui
                        .add(
                            egui::Button::new(RichText::new("Back to Repositories").color(pal.text))
                                .min_size(Vec2::new(140.0, 32.0)),
                        )
                        .clicked()
                    {
                        self.show_settings = false;
                    }
                });
            });

        if should_save {
            match self.settings_draft.apply_to_config(&self.config) {
                Ok(new_cfg) => {
                    let save_path = self
                        .config_path
                        .clone()
                        .or_else(|| Config::default_config_path().ok());
                    if let Some(path) = save_path {
                        match new_cfg.save_to_file(&path) {
                            Ok(()) => {
                                self.config = new_cfg;
                                self.config_path = Some(path.clone());
                                self.settings_error = None;
                                self.show_settings = false;
                                self.toast =
                                    Some((format!("Saved config to {}", path.display()), true));
                                self.trigger_scan(ctx, false);
                            }
                            Err(e) => {
                                self.settings_error = Some(format!("Failed to save: {:#}", e));
                            }
                        }
                    } else {
                        self.config = new_cfg;
                        self.show_settings = false;
                        self.trigger_scan(ctx, false);
                    }
                }
                Err(e) => {
                    self.settings_error = Some(e);
                }
            }
        }
    }
}

impl Drop for RepoMApp {
    fn drop(&mut self) {
        if let Some(rt) = self.rt.take() {
            // A blocking filesystem or Git inspection can outlive the window.
            rt.shutdown_background();
        }
    }
}

impl eframe::App for RepoMApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_events();

        let filtered = self.filtered_reports();

        self.render_bottom_bar(ctx);
        self.render_sidebar(ctx);
        if !self.show_settings {
            self.render_inspector(ctx);
            self.render_header_bar(ctx, filtered.len());
        } else {
            let pal = self.palette();
            egui::TopBottomPanel::top("settings_header")
                .exact_height(56.0)
                .show_separator_line(false)
                .frame(
                    egui::Frame::none()
                        .fill(pal.surface)
                        .inner_margin(Margin::symmetric(20.0, 10.0))
                        .stroke(Stroke::new(1.0_f32, pal.border_subtle)),
                )
                .show(ctx, |ui| {
                    ui.horizontal_centered(|ui| {
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new("Settings")
                                    .strong()
                                    .size(16.0)
                                    .color(pal.text),
                            );
                            ui.label(
                                RichText::new("Workspace paths, GitHub comparison, and privacy rules")
                                    .size(11.5)
                                    .color(pal.muted),
                            );
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("Back to Repositories")
                                            .size(12.0)
                                            .color(pal.text),
                                    )
                                    .min_size(Vec2::new(0.0, 30.0)),
                                )
                                .clicked()
                            {
                                self.show_settings = false;
                            }
                        });
                    });
                });
        }

        let pal = self.palette();
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(pal.canvas)
                    .inner_margin(Margin::symmetric(20.0, 16.0)),
            )
            .show(ctx, |ui| {
                if self.show_settings {
                    self.render_settings_page(ctx, ui);
                    return;
                }

                // Sleek dismissible GitHub callout banner (no raw markdown backticks)
                if let Some(gh_err) = self.github_error.clone() {
                    if !self.banner_dismissed {
                        let clean_err = gh_err.replace('`', "");
                        egui::Frame::none()
                            .fill(pal.amber_soft)
                            .rounding(Rounding::same(8.0))
                            .stroke(Stroke::new(1.0_f32, pal.amber))
                            .inner_margin(Margin::symmetric(12.0, 9.0))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    paint_dot(ui, pal.amber, 4.0);
                                    ui.label(
                                        RichText::new(clean_err)
                                            .size(12.0)
                                            .color(pal.text),
                                    );
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        if ui.small_button("Dismiss").clicked() {
                                            self.banner_dismissed = true;
                                        }
                                        if ui.small_button("Open Settings").clicked() {
                                            self.settings_draft =
                                                SettingsDraft::from_config(&self.config);
                                            self.settings_error = None;
                                            self.show_settings = true;
                                        }
                                    });
                                });
                            });
                        ui.add_space(12.0);
                    }
                }

                self.render_kpi_strip(ui);
                ui.add_space(14.0);
                self.render_table_card(ctx, ui, &filtered);
            });

        self.render_confirm_modal(ctx);
    }
}

async fn run_scan_task(
    cfg: &Config,
    enable_github: bool,
    token: Option<&str>,
    do_fetch: bool,
    progress: Option<(Sender<BgEvent>, egui::Context)>,
) -> anyhow::Result<(Vec<RepoReport>, Option<String>)> {
    let progress_enabled = progress.is_some();
    let notify = move |event| {
        if let Some((tx, ctx)) = &progress {
            let _ = tx.send(event);
            ctx.request_repaint();
        }
    };
    let scan_cfg = cfg.clone();
    let repo_paths = tokio::task::spawn_blocking(move || discover_repos(&scan_cfg)).await??;
    notify(BgEvent::ScanDiscovered(repo_paths.len()));

    let mut fetch_errors: HashMap<PathBuf, String> = HashMap::new();
    if do_fetch && !repo_paths.is_empty() {
        notify(BgEvent::ScanFetching);
        for (path, res) in fetch_repos_parallel(&repo_paths, 30).await {
            if let Err(err) = res {
                fetch_errors.insert(path, format!("fetch failed: {}", err));
            }
        }
    }

    let inspection_notify = notify.clone();
    let mut local_reports = tokio::task::spawn_blocking(move || {
        inspect_repos_with_progress(&repo_paths, |report| {
            if progress_enabled {
                inspection_notify(BgEvent::ScanInspected(Box::new(report.clone())));
            }
        })
    })
    .await?;
    for r in &mut local_reports {
        if let Some(ref p) = r.path {
            if let Some(err) = fetch_errors.get(p) {
                if r.error.is_none() {
                    r.error = Some(err.clone());
                }
            }
        }
    }

    let mut github_error = None;
    let final_reports = if enable_github {
        notify(BgEvent::ScanComparing);
        match GithubClient::new(token) {
            Ok(client) => match client
                .list_all_repos(cfg.github_user.as_deref(), &cfg.github_orgs)
                .await
            {
                Ok(gh_repos) => {
                    merge_local_and_github(local_reports, gh_repos, CompareOptions::default())
                }
                Err(err) => {
                    github_error = Some(err.to_string());
                    local_reports
                }
            },
            Err(err) => {
                github_error = Some(err.to_string());
                local_reports
            }
        }
    } else {
        local_reports
    };

    Ok((final_reports, github_error))
}

fn configure_enterprise_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();

    #[cfg(target_os = "windows")]
    {
        // Use Segoe UI Semibold fallback or Regular for crisp anti-aliased rendering in both Dark and Light modes
        if let Ok(segoe_bytes) = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") {
            fonts
                .font_data
                .insert("segoe_ui".to_owned(), FontData::from_owned(segoe_bytes));
            if let Some(fam) = fonts.families.get_mut(&FontFamily::Proportional) {
                fam.insert(0, "segoe_ui".to_owned());
            }
        }
        if let Ok(consola_bytes) = std::fs::read(r"C:\Windows\Fonts\consola.ttf") {
            fonts
                .font_data
                .insert("consolas".to_owned(), FontData::from_owned(consola_bytes));
            if let Some(fam) = fonts.families.get_mut(&FontFamily::Monospace) {
                fam.insert(0, "consolas".to_owned());
            }
        }
    }

    ctx.set_fonts(fonts);
}

fn settings_section_card(
    ui: &mut egui::Ui,
    title: &str,
    subtitle: &str,
    pal: &Palette,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    egui::Frame::none()
        .fill(pal.surface)
        .rounding(Rounding::same(10.0))
        .stroke(Stroke::new(1.0_f32, pal.border))
        .inner_margin(Margin::same(16.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(RichText::new(title).strong().size(14.5).color(pal.text));
            ui.label(RichText::new(subtitle).size(11.5).color(pal.muted));
            ui.add_space(10.0);
            add_contents(ui);
        });
}

fn kpi_card_frame(pal: &Palette, active: bool) -> egui::Frame {
    egui::Frame::none()
        .fill(pal.surface)
        .rounding(Rounding::same(10.0))
        .stroke(Stroke::new(
            if active { 1.5_f32 } else { 1.0_f32 },
            if active { pal.accent } else { pal.border },
        ))
        .inner_margin(Margin::symmetric(14.0, 12.0))
}

fn sidebar_nav_item(
    ui: &mut egui::Ui,
    label: &str,
    count: usize,
    dot_color: Color32,
    active: bool,
    pal: &Palette,
) -> bool {
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 25.0), Sense::click());

    let bg = if active {
        pal.accent_soft
    } else if resp.hovered() {
        pal.hover
    } else {
        Color32::TRANSPARENT
    };

    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, Rounding::same(6.0), bg);
    }
    if active {
        let bar = Rect::from_min_size(
            egui::pos2(rect.min.x, rect.min.y + 5.0),
            Vec2::new(3.0, rect.height() - 10.0),
        );
        ui.painter()
            .rect_filled(bar, Rounding::same(1.5), pal.accent);
    }

    let dot_center = egui::pos2(rect.min.x + 14.0, rect.center().y);
    let actual_dot = if count == 0 && !active {
        pal.border
    } else {
        dot_color
    };
    ui.painter().circle_filled(dot_center, 3.5, actual_dot);

    let text_color = if active {
        pal.text
    } else if count == 0 {
        pal.muted
    } else {
        pal.text_secondary
    };

    ui.painter().text(
        egui::pos2(rect.min.x + 26.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        FontId::new(12.5, FontFamily::Proportional),
        text_color,
    );

    let count_color = if active { pal.accent } else { pal.muted };
    ui.painter().text(
        egui::pos2(rect.max.x - 10.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        count.to_string(),
        FontId::new(11.5, FontFamily::Monospace),
        count_color,
    );

    resp.clicked()
}

fn toggle_switch(ui: &mut egui::Ui, on: &mut bool, enabled: bool, pal: &Palette) -> bool {
    let desired = Vec2::new(34.0, 18.0);
    let (rect, resp) = ui.allocate_exact_size(
        desired,
        if enabled { Sense::click() } else { Sense::hover() },
    );
    let clicked = enabled && resp.clicked();
    if clicked {
        *on = !*on;
    }

    let bg = if *on { pal.accent } else { pal.border };
    ui.painter().rect_filled(rect, Rounding::same(9.0), bg);
    let knob_x = if *on {
        rect.max.x - 9.0
    } else {
        rect.min.x + 9.0
    };
    ui.painter()
        .circle_filled(egui::pos2(knob_x, rect.center().y), 6.5, Color32::WHITE);

    clicked
}

fn filter_chip(ui: &mut egui::Ui, label: &str, value: &mut bool, pal: &Palette) {
    let fg = if *value { pal.accent } else { pal.text_secondary };
    let bg = if *value { pal.accent_soft } else { pal.elevated };
    let stroke = if *value { pal.accent } else { pal.border };
    let text = if *value {
        format!("[x] {}", label)
    } else {
        label.to_string()
    };
    let btn = egui::Button::new(RichText::new(text).size(11.5).color(fg))
        .fill(bg)
        .stroke(Stroke::new(1.0_f32, stroke))
        .rounding(Rounding::same(6.0))
        .min_size(Vec2::new(0.0, 26.0));
    if ui.add(btn).clicked() {
        *value = !*value;
    }
}

fn paint_health_bar(ui: &mut egui::Ui, s: &Summary, pal: &Palette) {
    let desired = Vec2::new(ui.available_width(), 6.0);
    let (rect, _) = ui.allocate_exact_size(desired, Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, Rounding::same(3.0), pal.elevated);

    if s.total == 0 {
        return;
    }

    let segs = [
        (s.clean, pal.green),
        (s.dirty + s.diverged, pal.red),
        (s.ahead + s.untracked, pal.amber),
        (s.behind + s.not_cloned, pal.cyan),
        (
            s.stashed + s.no_upstream + s.no_remote + s.not_on_github,
            pal.purple,
        ),
    ];
    let sum: usize = segs.iter().map(|(c, _)| *c).sum();
    let denom = sum.max(s.total) as f32;
    let mut x = rect.min.x;
    for (count, color) in segs {
        if count == 0 {
            continue;
        }
        let w = (count as f32 / denom) * rect.width();
        let seg_rect = Rect::from_min_size(
            egui::pos2(x, rect.min.y),
            Vec2::new(w.max(2.0), rect.height()),
        )
        .intersect(rect);
        painter.rect_filled(seg_rect, Rounding::same(2.0), color);
        x += w;
    }
}

fn paint_dot(ui: &mut egui::Ui, color: Color32, radius: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(radius * 2.0 + 2.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), radius, color);
}

fn paint_brand_badge(painter: &egui::Painter, rect: Rect, accent: Color32) {
    painter.rect_filled(rect, Rounding::same(8.0), accent);
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "RM",
        FontId::new(13.0, FontFamily::Proportional),
        Color32::WHITE,
    );
}

fn draw_status_badge(
    ui: &mut egui::Ui,
    label: &str,
    fg: Color32,
    bg: Color32,
) -> egui::Response {
    egui::Frame::none()
        .fill(bg)
        .rounding(Rounding::same(10.0))
        .inner_margin(Margin::symmetric(7.0, 2.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                paint_dot(ui, fg, 2.8);
                ui.label(RichText::new(label).size(10.5).strong().color(fg));
            });
        })
        .response
}

fn draw_tag(ui: &mut egui::Ui, label: &str, fg: Color32, bg: Color32) {
    egui::Frame::none()
        .fill(bg)
        .rounding(Rounding::same(5.0))
        .inner_margin(Margin::symmetric(5.0, 1.0))
        .show(ui, |ui| {
            ui.label(RichText::new(label).size(10.0).color(fg));
        });
}

fn kv_row(ui: &mut egui::Ui, label: &str, value: &str, muted: Color32) {
    ui.label(RichText::new(label).color(muted).size(12.0));
    ui.label(RichText::new(value).font(FontId::new(12.0, FontFamily::Monospace)));
    ui.end_row();
}

fn truncate_middle(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars || max_chars < 6 {
        return s.to_string();
    }
    let head = (max_chars - 3) / 2;
    let tail = max_chars - 3 - head;
    let start: String = s.chars().take(head).collect();
    let end: String = s.chars().skip(count - tail).collect();
    format!("{}...{}", start, end)
}

fn github_web_url(repo: &RepoReport) -> Option<String> {
    if let Some(ref full) = repo.github_full_name {
        return Some(format!("https://github.com/{}", full));
    }
    if let Some(ref url) = repo.remote_url {
        if let Some(slug) = crate::compare::normalize_github_slug(url) {
            return Some(format!("https://github.com/{}", slug));
        }
    }
    None
}

/// Generate a crisp 32x32 RGBA desktop window icon (blue rounded square with git-branch nodes).
fn build_app_icon() -> egui::IconData {
    let size = 32usize;
    let mut rgba = vec![0u8; size * size * 4];

    for y in 0..size {
        for x in 0..size {
            let idx = (y * size + x) * 4;
            let fx = x as f32 + 0.5;
            let fy = y as f32 + 0.5;
            let r = 7.0f32;
            let dx = (fx - r).min(0.0).max(fx - (size as f32 - r));
            let dy = (fy - r).min(0.0).max(fy - (size as f32 - r));
            let corner_dist = (dx * dx + dy * dy).sqrt();
            if corner_dist > r {
                continue; // transparent corner
            }

            // Clean enterprise blue gradient
            let t = (fx + fy) / (2.0 * size as f32);
            let mut red = (37.0 + (20.0 * t)) as u8;
            let mut green = (99.0 + (45.0 * t)) as u8;
            let mut blue = (235.0 - (15.0 * t)) as u8;

            // Simple crisp white git-branch motif (vertical stem + branch + 3 nodes)
            let on_stem = (fx - 11.0).abs() <= 1.4 && (9.0..=23.0).contains(&fy);
            let on_branch = (fx - 21.0).abs() <= 1.4 && (11.0..=17.0).contains(&fy);
            let on_join = (fy - 17.0).abs() <= 1.4 && (11.0..=21.0).contains(&fx);
            let d1 = ((fx - 11.0).powi(2) + (fy - 9.0).powi(2)).sqrt();
            let d2 = ((fx - 11.0).powi(2) + (fy - 23.0).powi(2)).sqrt();
            let d3 = ((fx - 21.0).powi(2) + (fy - 11.0).powi(2)).sqrt();
            if on_stem || on_branch || on_join || d1 <= 2.9 || d2 <= 2.9 || d3 <= 2.9 {
                red = 255;
                green = 255;
                blue = 255;
            }

            rgba[idx] = red;
            rgba[idx + 1] = green;
            rgba[idx + 2] = blue;
            rgba[idx + 3] = 255;
        }
    }

    egui::IconData {
        rgba,
        width: size as u32,
        height: size as u32,
    }
}

/// Launch the native pure-Rust desktop window.
pub fn run_desktop_app() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RepoM — Enterprise Repository Sync Manager")
            .with_icon(Arc::new(build_app_icon()))
            .with_inner_size([1360.0, 840.0])
            .with_min_inner_size([960.0, 600.0]),
        ..Default::default()
    };

    eframe::run_native(
        "RepoM",
        options,
        Box::new(|cc| Ok(Box::new(RepoMApp::new(cc)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn scan_publishes_discovery_and_each_local_report() {
        let workspace = tempfile::tempdir().unwrap();
        for name in ["first", "second"] {
            git2::Repository::init(workspace.path().join(name)).unwrap();
        }
        let cfg = Config::from_root(workspace.path().to_path_buf());
        let (tx, rx) = mpsc::channel();

        let (reports, github_error) =
            run_scan_task(&cfg, false, None, false, Some((tx, egui::Context::default())))
                .await
                .unwrap();

        let mut events = rx.try_iter();
        assert!(matches!(events.next(), Some(BgEvent::ScanDiscovered(2))));
        let mut names: Vec<_> = events
            .map(|event| match event {
                BgEvent::ScanInspected(report) => report.name,
                other => panic!("unexpected scan event: {other:?}"),
            })
            .collect();
        names.sort();
        assert_eq!(names, ["first", "second"]);
        assert_eq!(reports.len(), 2);
        assert!(github_error.is_none());
    }
}
