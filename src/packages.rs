use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

use crate::logging::Logger;
use crate::privilege::{InvokingUser, command_as_user};

/// Parse `LC_ALL=C pacman -Qii` output and return `<pkg>\t<path>\n` lines for modified backup files.
pub fn parse_pacman_modified_files(qii_output: &str) -> String {
    let mut out = String::new();
    let mut current_pkg: Option<&str> = None;

    for line in qii_output.lines() {
        if let Some(rest) = line.strip_prefix("Name")
            && let Some((_, rhs)) = rest.split_once(':')
        {
            let pkg = rhs.trim();
            if !pkg.is_empty() {
                current_pkg = Some(pkg);
            }
            continue;
        }

        let trimmed = line.trim();
        if let Some(before_tag) = trimmed.strip_suffix("[modified]")
            && let Some(pkg) = current_pkg
        {
            let path_part = before_tag.trim();
            let path = if let Some(rest) = path_part.strip_prefix("Backup Files")
                && let Some((_, rhs)) = rest.split_once(':')
            {
                rhs.trim()
            } else {
                path_part
            };
            if !path.is_empty() {
                out.push_str(pkg);
                out.push('\t');
                out.push_str(path);
                out.push('\n');
            }
        }
    }

    out
}

/// Dump pacman, flatpak, omarchy, and systemd unit lists into `pk_dir`.
pub fn dump_package_lists(pk_dir: &Path, user: &InvokingUser, logger: &Logger) -> Result<()> {
    logger.log("Dumping package lists");

    capture_required(
        Command::new("pacman").arg("-Qqen"),
        &pk_dir.join("pacman-native.txt"),
        "pacman -Qqen",
    )?;
    capture_best_effort(
        Command::new("pacman").arg("-Qqem"),
        &pk_dir.join("pacman-aur.txt"),
    )?;
    capture_required(
        Command::new("pacman").arg("-Qqe"),
        &pk_dir.join("pacman-all-explicit.txt"),
        "pacman -Qqe",
    )?;
    capture_required(
        Command::new("pacman").arg("-Qqd"),
        &pk_dir.join("pacman-deps.txt"),
        "pacman -Qqd",
    )?;
    capture_required(
        Command::new("pacman").arg("-Q"),
        &pk_dir.join("pacman-versions.txt"),
        "pacman -Q",
    )?;

    let modified = match Command::new("pacman")
        .env("LC_ALL", "C")
        .arg("-Qii")
        .stderr(Stdio::null())
        .output()
    {
        Ok(out) => parse_pacman_modified_files(&String::from_utf8_lossy(&out.stdout)),
        Err(_) => String::new(),
    };
    fs::write(pk_dir.join("pacman-modified-files.txt"), modified)
        .context("failed to write pacman-modified-files.txt")?;

    if command_exists("flatpak") {
        let mut apps = command_as_user("flatpak", user);
        apps.args(["list", "--app", "--columns=application,origin"]);
        capture_best_effort(&mut apps, &pk_dir.join("flatpak-apps.txt"))?;

        let mut remotes = command_as_user("flatpak", user);
        remotes.args(["remotes", "--columns=name,url"]);
        capture_best_effort(&mut remotes, &pk_dir.join("flatpak-remotes.txt"))?;
    }

    capture_best_effort(
        Command::new("omarchy").arg("version"),
        &pk_dir.join("omarchy-version.txt"),
    )?;

    capture_best_effort(
        Command::new("systemctl").args(["list-unit-files", "--state=enabled"]),
        &pk_dir.join("systemd-enabled.txt"),
    )?;

    let mut user_units = command_as_user("systemctl", user);
    user_units
        .env("XDG_RUNTIME_DIR", format!("/run/user/{}", user.uid))
        .args(["--user", "list-unit-files", "--state=enabled"]);
    capture_best_effort(&mut user_units, &pk_dir.join("systemd-user-enabled.txt"))?;

    Ok(())
}

fn capture_required(cmd: &mut Command, dest: &Path, label: &str) -> Result<()> {
    let out = cmd
        .output()
        .with_context(|| format!("failed to execute {label}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("{label} failed ({}): {}", out.status, stderr.trim());
    }
    fs::write(dest, &out.stdout).with_context(|| format!("failed to write {}", dest.display()))?;
    Ok(())
}

fn capture_best_effort(cmd: &mut Command, dest: &Path) -> Result<()> {
    let bytes = match cmd.stderr(Stdio::null()).output() {
        Ok(out) => out.stdout,
        Err(_) => Vec::new(),
    };
    fs::write(dest, bytes).with_context(|| format!("failed to write {}", dest.display()))?;
    Ok(())
}

fn command_exists(program: &str) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(program);
        if candidate.is_file() {
            return true;
        }
    }
    false
}
