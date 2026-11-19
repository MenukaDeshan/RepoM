use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub root: PathBuf,
    #[serde(default)]
    pub github_user: Option<String>,
    #[serde(default)]
    pub github_orgs: Vec<String>,
    #[serde(default = "default_token_env")]
    pub token_env: String,
    #[serde(default)]
    pub scan: ScanConfig,
    #[serde(default)]
    pub machine: MachineConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanConfig {
    #[serde(default = "default_max_depth")]
    pub max_depth: usize,
    #[serde(default = "default_exclude")]
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineConfig {
    #[serde(default = "default_machine_name")]
    pub name: String,
    #[serde(default)]
    pub skip_folders: Vec<String>,
}

fn default_token_env() -> String {
    "GITHUB_TOKEN".to_string()
}

fn default_max_depth() -> usize {
    3
}

fn default_exclude() -> Vec<String> {
    vec![
        "node_modules".into(),
        "99_Archive".into(),
        ".next".into(),
        "target".into(),
        ".venv".into(),
        "dist".into(),
        "build".into(),
    ]
}

fn default_machine_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            max_depth: default_max_depth(),
            exclude: default_exclude(),
        }
    }
}

impl Default for MachineConfig {
    fn default() -> Self {
        Self {
            name: default_machine_name(),
            skip_folders: Vec::new(),
        }
    }
}

impl Config {
    /// Creates an ad-hoc configuration when running with `--root` and no config file.
    pub fn from_root(root: PathBuf) -> Self {
        Self {
            root,
            github_user: None,
            github_orgs: Vec::new(),
            token_env: default_token_env(),
            scan: ScanConfig::default(),
            machine: MachineConfig::default(),
        }
    }

    /// Default platform-specific configuration file path:
    /// - Windows: `%APPDATA%\repom\config.toml`
    /// - Unix/macOS: `~/.config/repom/config.toml`
    pub fn default_config_path() -> Result<PathBuf> {
        let base = dirs::config_dir()
            .context("could not determine platform configuration directory")?;
        Ok(base.join("repom").join("config.toml"))
    }

    /// Resolve which config file to load:
    /// 1. Explicit `--config` path if supplied.
    /// 2. `./repom.toml` in the current working directory if it exists.
    /// 3. Platform default config path (`~/.config/repom/config.toml` or `%APPDATA%\repom\config.toml`).
    pub fn resolve_path(explicit: Option<&Path>) -> Result<Option<PathBuf>> {
        if let Some(p) = explicit {
            return Ok(Some(p.to_path_buf()));
        }
        let local = PathBuf::from("repom.toml");
        if local.is_file() {
            return Ok(Some(local));
        }
        let default_path = Self::default_config_path()?;
        if default_path.is_file() {
            return Ok(Some(default_path));
        }
        Ok(None)
    }

    /// Load and validate a configuration file from disk.
    pub fn load_from_file(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read config file {}", path.display()))?;
        Self::parse_str(&raw)
            .with_context(|| format!("invalid configuration in {}", path.display()))
    }

    /// Parse and validate configuration TOML text.
    pub fn parse_str(raw: &str) -> Result<Self> {
        // Explicit security check before typed deserialization so users get a clear explanation
        // if they accidentally attempt to put a raw GitHub token into the TOML file.
        let value: toml::Value = toml::from_str(raw).context("failed to parse TOML syntax")?;
        if let Some(table) = value.as_table() {
            for forbidden in ["token", "github_token", "pat", "access_token", "secret"] {
                if table.contains_key(forbidden) {
                    bail!(
                        "security error: '{}' must not be stored in the config file. \
                         Set `token_env = \"GITHUB_TOKEN\"` and export the token in your environment instead.",
                        forbidden
                    );
                }
            }
        }

        let cfg: Config = toml::from_str(raw)?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Load config with CLI overrides (`--config`, `--root`, `--max-depth`).
    pub fn load_with_overrides(
        explicit_config: Option<&Path>,
        root_override: Option<&Path>,
        max_depth_override: Option<usize>,
    ) -> Result<Self> {
        let mut cfg = match Self::resolve_path(explicit_config)? {
            Some(path) => Self::load_from_file(&path)?,
            None => {
                if let Some(root) = root_override {
                    Self::from_root(root.to_path_buf())
                } else {
                    let default_path = Self::default_config_path()?;
                    bail!(
                        "no config file found (looked for ./repom.toml and {}). \
                         Run `repom init` to create one, or pass `--root <PATH>`.",
                        default_path.display()
                    );
                }
            }
        };

        if let Some(root) = root_override {
            cfg.root = root.to_path_buf();
        }
        if let Some(depth) = max_depth_override {
            cfg.scan.max_depth = depth;
        }
        cfg.validate()?;
        Ok(cfg)
    }

    /// Validate configuration invariants.
    pub fn validate(&self) -> Result<()> {
        if self.root.as_os_str().is_empty() {
            bail!("`root` directory cannot be empty");
        }
        if self.scan.max_depth == 0 {
            bail!("`scan.max_depth` must be at least 1");
        }
        // Validate token_env looks like a safe environment variable name (never a raw token)
        if self.token_env.is_empty()
            || !self
                .token_env
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            bail!(
                "`token_env` must be a valid environment variable name (e.g. GITHUB_TOKEN), not a secret value"
            );
        }
        if self.token_env.starts_with("ghp_")
            || self.token_env.starts_with("github_pat_")
            || self.token_env.starts_with("gho_")
        {
            bail!(
                "security error: `token_env` looks like a raw GitHub token! \
                 Store the environment variable NAME (e.g. `token_env = \"GITHUB_TOKEN\"`), never the token itself."
            );
        }
        Ok(())
    }

    /// Read the GitHub API token from the configured environment variable.
    /// Returns `None` if the variable is unset or empty.
    pub fn read_github_token(&self) -> Option<String> {
        std::env::var(&self.token_env)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    }

    /// Load existing config if present, or fall back to a sensible default root directory
    /// so double-clicking `repom.exe` works cleanly on first launch.
    pub fn load_or_default() -> (Self, Option<PathBuf>) {
        if let Ok(Some(path)) = Self::resolve_path(None) {
            if let Ok(cfg) = Self::load_from_file(&path) {
                return (cfg, Some(path));
            }
        }
        let fallback_root = std::env::current_dir()
            .ok()
            .map(|mut d| {
                // If launched by double-clicking target/release/repom.exe or target/debug/repom.exe,
                // step up out of `target/<profile>` first.
                let is_target_profile = d
                    .file_name()
                    .map(|n| n == "release" || n == "debug")
                    .unwrap_or(false)
                    && d.parent()
                        .and_then(|p| p.file_name())
                        .map(|n| n == "target")
                        .unwrap_or(false);
                if is_target_profile {
                    if let Some(proj) = d.parent().and_then(|p| p.parent()) {
                        d = proj.to_path_buf();
                    }
                }
                // If inside a single project/repo directory (e.g. Devweb/RepoM), scan its parent workspace.
                if d.join(".git").exists() || d.join("Cargo.toml").exists() {
                    if let Some(parent) = d.parent() {
                        d = parent.to_path_buf();
                    }
                }
                d
            })
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        (Self::from_root(fallback_root), None)
    }

    /// Persist configuration to disk (validates first; never writes raw tokens).
    pub fn save_to_file(&self, path: &Path) -> Result<()> {
        self.validate()?;
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).with_context(|| {
                    format!("failed to create config directory {}", parent.display())
                })?;
            }
        }
        let serialized = toml::to_string_pretty(self).context("failed to serialize config TOML")?;
        fs::write(path, serialized)
            .with_context(|| format!("failed to write config file {}", path.display()))
    }

    /// Generate a starter TOML template for `repom init`.
    pub fn default_template(root_hint: &str) -> String {
        format!(
            r#"# RepoM configuration
# Documentation: https://github.com/your-username/repom

root = "{root}"
github_user = "your-username"
github_orgs = []           # optional, e.g. ["veltrion"]
token_env = "GITHUB_TOKEN" # never store the token itself in this file

[scan]
max_depth = 3
exclude = ["node_modules", "99_Archive", ".next", "target", ".venv", "dist", "build"]

[machine]
name = "{machine}"
skip_folders = []          # e.g. ["02_Rainco"] — never scanned or reported on this machine
"#,
            root = root_hint.replace('\\', "/"),
            machine = default_machine_name()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_config() {
        let toml = r#"
root = "C:/Work"
github_user = "alice"
github_orgs = ["veltrion"]
token_env = "GITHUB_TOKEN"

[scan]
max_depth = 4
exclude = ["node_modules", "target"]

[machine]
name = "pc"
skip_folders = ["02_Rainco"]
"#;
        let cfg = Config::parse_str(toml).unwrap();
        assert_eq!(cfg.root, PathBuf::from("C:/Work"));
        assert_eq!(cfg.github_user.as_deref(), Some("alice"));
        assert_eq!(cfg.github_orgs, vec!["veltrion"]);
        assert_eq!(cfg.scan.max_depth, 4);
        assert_eq!(cfg.machine.name, "pc");
        assert_eq!(cfg.machine.skip_folders, vec!["02_Rainco"]);
    }

    #[test]
    fn rejects_hardcoded_token_field() {
        let toml = r#"
root = "C:/Work"
token = "ghp_secret12345"
"#;
        let err = Config::parse_str(toml).unwrap_err();
        assert!(err.to_string().contains("security error"));
    }

    #[test]
    fn rejects_token_in_token_env() {
        let toml = r#"
root = "C:/Work"
token_env = "ghp_1234567890abcdef"
"#;
        let err = Config::parse_str(toml).unwrap_err();
        assert!(err.to_string().contains("security error"));
    }
}
