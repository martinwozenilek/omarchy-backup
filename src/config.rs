use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::constants::{
    DEFAULT_CONFIG_FILENAME, DEFAULT_DEST_DIR, DEFAULT_FS_LABEL, DEFAULT_KEEP_DELETED_DAYS,
    DEFAULT_KEEP_LOGS, DEFAULT_LUKS_LABEL, DEFAULT_MARKER, DEFAULT_MIN_FREE_GIB,
    DEFAULT_XDG_CONFIG_REL, RESERVED_DEST_EXACT, RESERVED_DEST_PREFIXES,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub fs_label: String,
    pub luks_label: String,
    pub marker: String,
    pub dest_dir: String,
    pub keep_deleted_days: u64,
    pub keep_logs: usize,
    pub min_free_gib: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            fs_label: DEFAULT_FS_LABEL.to_string(),
            luks_label: DEFAULT_LUKS_LABEL.to_string(),
            marker: DEFAULT_MARKER.to_string(),
            dest_dir: DEFAULT_DEST_DIR.to_string(),
            keep_deleted_days: DEFAULT_KEEP_DELETED_DAYS,
            keep_logs: DEFAULT_KEEP_LOGS,
            min_free_gib: DEFAULT_MIN_FREE_GIB,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub path: PathBuf,
    pub dest: String,
    pub optional: bool,
    pub vfat: bool,
    pub one_file_system: bool,
    pub excludes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub settings: Settings,
    pub sources: Vec<Source>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    settings: RawSettings,
    #[serde(default)]
    source: Option<Vec<RawSource>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSettings {
    fs_label: Option<String>,
    luks_label: Option<String>,
    marker: Option<String>,
    dest_dir: Option<String>,
    keep_deleted_days: Option<u64>,
    keep_logs: Option<usize>,
    min_free_gib: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSource {
    path: String,
    dest: String,
    #[serde(default)]
    optional: bool,
    #[serde(default)]
    vfat: bool,
    #[serde(default = "default_true")]
    one_file_system: bool,
    #[serde(default)]
    excludes: Vec<String>,
}

const fn default_true() -> bool {
    true
}

/// Discover the config file path in priority order:
/// 1. Explicit `--config FILE`
/// 2. `./omarchy-backup.toml`
/// 3. `<exe-dir>/omarchy-backup.toml`
/// 4. `<user_home>/.config/omarchy-backup/config.toml`
pub fn discover_config_path(explicit: Option<&Path>, user_home: &Path) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }

    let mut candidates = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join(DEFAULT_CONFIG_FILENAME));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let exe_cfg = dir.join(DEFAULT_CONFIG_FILENAME);
        if !candidates.contains(&exe_cfg) {
            candidates.push(exe_cfg);
        }
    }
    let xdg_cfg = user_home.join(DEFAULT_XDG_CONFIG_REL);
    if !candidates.contains(&xdg_cfg) {
        candidates.push(xdg_cfg);
    }

    for candidate in &candidates {
        if candidate.is_file() {
            return Ok(candidate.clone());
        }
    }

    let searched = candidates
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    bail!("No config file found (searched: {searched}). Pass --config FILE.");
}

/// Load and validate the full backup configuration from `cfg_path`.
pub fn load_config(cfg_path: &Path, user_home: &Path) -> Result<Config> {
    let raw_text = fs::read_to_string(cfg_path)
        .with_context(|| format!("Config {}: failed to read file", cfg_path.display()))?;
    parse_and_validate(&raw_text, cfg_path, user_home)
}

/// Load `[settings]` for `format-usb` if a config file is provided or discovered,
/// falling back to `Settings::default()` when no config file is found.
pub fn load_settings_for_format(
    explicit: Option<&Path>,
    user_home: &Path,
) -> Result<(Settings, Option<PathBuf>)> {
    let path = match explicit {
        Some(p) => p.to_path_buf(),
        None => match discover_config_path(None, user_home) {
            Ok(p) => p,
            Err(_) => return Ok((Settings::default(), None)),
        },
    };
    let raw_text = fs::read_to_string(&path)
        .with_context(|| format!("Config {}: failed to read file", path.display()))?;
    let raw: RawConfig = toml::from_str(&raw_text)
        .with_context(|| format!("Config {}: invalid TOML", path.display()))?;
    let settings = validate_settings(raw.settings, &path)?;
    Ok((settings, Some(path)))
}

pub fn parse_and_validate(raw_text: &str, cfg_path: &Path, user_home: &Path) -> Result<Config> {
    let raw: RawConfig = toml::from_str(raw_text)
        .with_context(|| format!("Config {}: invalid TOML", cfg_path.display()))?;
    let settings = validate_settings(raw.settings, cfg_path)?;

    let raw_sources = match raw.source {
        Some(list) if !list.is_empty() => list,
        _ => bail!("Config {}: no [[source]] entries", cfg_path.display()),
    };

    let home_str = user_home
        .to_str()
        .with_context(|| format!("User home path is not valid UTF-8: {}", user_home.display()))?;

    let mut seen_dests: Vec<String> = Vec::with_capacity(raw_sources.len());
    let mut sources = Vec::with_capacity(raw_sources.len());

    for (idx, s) in raw_sources.into_iter().enumerate() {
        let num = idx + 1;
        if contains_forbidden_path_chars(&s.path) || contains_forbidden_path_chars(&s.dest) {
            bail!(
                "Config {}: source {num}: tabs and newlines are not allowed in path or dest",
                cfg_path.display()
            );
        }

        let expanded_path = expand_tilde(&s.path, home_str);
        if !expanded_path.starts_with('/') {
            bail!(
                "Config {}: source {num}: path must be absolute or start with ~",
                cfg_path.display()
            );
        }

        let norm_path = posix_normpath(&expanded_path);
        let norm_dest = posix_normpath(&s.dest);

        if norm_dest.starts_with('/')
            || norm_dest == "."
            || norm_dest.split('/').any(|seg| seg == "..")
        {
            bail!(
                "Config {}: source {num}: dest must be a relative path inside the backup",
                cfg_path.display()
            );
        }

        let first_seg = norm_dest.split('/').next().unwrap_or("");
        if RESERVED_DEST_PREFIXES.contains(&first_seg)
            || RESERVED_DEST_EXACT.contains(&norm_dest.as_str())
        {
            bail!(
                "Config {}: source {num}: dest {norm_dest:?} is reserved",
                cfg_path.display()
            );
        }

        for prev in &seen_dests {
            if norm_dest == *prev
                || norm_dest.starts_with(&format!("{prev}/"))
                || prev.starts_with(&format!("{norm_dest}/"))
            {
                bail!(
                    "Config {}: source {num}: dest {norm_dest:?} overlaps {prev:?}",
                    cfg_path.display()
                );
            }
        }
        seen_dests.push(norm_dest.clone());

        for ex in &s.excludes {
            if ex.is_empty() || ex.contains('\n') || ex.contains('\r') {
                bail!(
                    "Config {}: source {num}: excludes must be a list of non-empty strings without newlines",
                    cfg_path.display()
                );
            }
        }

        sources.push(Source {
            path: PathBuf::from(norm_path),
            dest: norm_dest,
            optional: s.optional,
            vfat: s.vfat,
            one_file_system: s.one_file_system,
            excludes: s.excludes,
        });
    }

    Ok(Config { settings, sources })
}

fn validate_settings(raw: RawSettings, cfg_path: &Path) -> Result<Settings> {
    let mut settings = Settings::default();

    if let Some(v) = raw.fs_label {
        validate_label("fs_label", &v, cfg_path)?;
        settings.fs_label = v;
    }
    if let Some(v) = raw.luks_label {
        validate_label("luks_label", &v, cfg_path)?;
        settings.luks_label = v;
    }
    if let Some(v) = raw.marker {
        if v.is_empty()
            || v == "."
            || v == ".."
            || v.contains('/')
            || contains_forbidden_path_chars(&v)
        {
            bail!(
                "Config {}: settings.marker must be a single valid filename",
                cfg_path.display()
            );
        }
        settings.marker = v;
    }
    if let Some(v) = raw.dest_dir {
        if contains_forbidden_path_chars(&v) {
            bail!(
                "Config {}: settings.dest_dir must not contain tabs or newlines",
                cfg_path.display()
            );
        }
        let norm = posix_normpath(&v);
        if norm.starts_with('/') || norm == "." || norm.split('/').any(|seg| seg == "..") {
            bail!(
                "Config {}: settings.dest_dir must be a relative path",
                cfg_path.display()
            );
        }
        settings.dest_dir = norm;
    }
    if let Some(v) = raw.keep_deleted_days {
        settings.keep_deleted_days = v;
    }
    if let Some(v) = raw.keep_logs {
        if v == 0 {
            bail!(
                "Config {}: settings.keep_logs must be at least 1",
                cfg_path.display()
            );
        }
        settings.keep_logs = v;
    }
    if let Some(v) = raw.min_free_gib {
        settings.min_free_gib = v;
    }

    Ok(settings)
}

fn validate_label(name: &str, value: &str, cfg_path: &Path) -> Result<()> {
    if value.is_empty() || value.contains('/') || contains_forbidden_path_chars(value) {
        bail!(
            "Config {}: settings.{name} must be a non-empty label without slashes or whitespace control chars",
            cfg_path.display()
        );
    }
    Ok(())
}

fn contains_forbidden_path_chars(s: &str) -> bool {
    s.contains('\t') || s.contains('\n') || s.contains('\r')
}

fn expand_tilde(path: &str, user_home: &str) -> String {
    if path == "~" {
        user_home.to_string()
    } else if let Some(rest) = path.strip_prefix("~/") {
        format!("{}/{}", user_home.trim_end_matches('/'), rest)
    } else {
        path.to_string()
    }
}

/// Lexical POSIX path normalization equivalent to Python's `posixpath.normpath`.
pub fn posix_normpath(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let initial_slashes = if path.starts_with('/') { 1 } else { 0 };
    let mut comps: Vec<&str> = Vec::new();
    for comp in path.split('/') {
        if comp.is_empty() || comp == "." {
            continue;
        }
        if comp != ".." {
            comps.push(comp);
        } else if initial_slashes == 0 && (comps.is_empty() || comps.last() == Some(&"..")) {
            comps.push("..");
        } else if !comps.is_empty() {
            comps.pop();
        }
    }

    let joined = comps.join("/");
    if initial_slashes > 0 {
        format!("/{joined}")
    } else if joined.is_empty() {
        ".".to_string()
    } else {
        joined
    }
}
