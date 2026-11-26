use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};
use nix::unistd::{User, geteuid};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvokingUser {
    pub name: String,
    pub home: PathBuf,
    pub uid: u32,
    pub gid: u32,
}

/// If the process is not running as root (EUID 0), replace the current process
/// with `sudo <current_exe> <original args...>`.
pub fn ensure_root() -> Result<()> {
    if geteuid().is_root() {
        return Ok(());
    }
    let exe = std::env::current_exe().context("failed to resolve current executable path")?;
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let err = Command::new("sudo").arg(&exe).args(&args).exec();
    Err(anyhow::Error::new(err).context("failed to re-exec via sudo"))
}

/// Resolve the non-root invoking user (from `SUDO_USER`, falling back to `logname`).
pub fn resolve_invoking_user() -> Result<InvokingUser> {
    let candidate = std::env::var("SUDO_USER")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(try_logname)
        .unwrap_or_default();

    if candidate.is_empty() || candidate == "root" {
        bail!(
            "Cannot determine the user to back up. Run as a normal user (the program uses sudo itself)."
        );
    }

    lookup_user(&candidate)?.ok_or_else(|| {
        anyhow::anyhow!(
            "Cannot determine the user to back up. Run as a normal user (the program uses sudo itself)."
        )
    })
}

/// Resolve a user home directory without requiring root (for `check` and `format-usb`).
pub fn resolve_home_best_effort() -> PathBuf {
    if let Ok(sudo_user) = std::env::var("SUDO_USER")
        && !sudo_user.is_empty()
        && sudo_user != "root"
        && let Ok(Some(u)) = lookup_user(&sudo_user)
    {
        return u.home;
    }
    if let Some(home) = std::env::var_os("HOME")
        && !home.is_empty()
    {
        return PathBuf::from(home);
    }
    if let Ok(Some(u)) = User::from_uid(geteuid()) {
        return u.dir;
    }
    PathBuf::from("/root")
}

/// Build a `Command` configured to run as `user` when currently running as root.
pub fn command_as_user(program: &str, user: &InvokingUser) -> Command {
    let mut cmd = Command::new(program);
    if geteuid().is_root() {
        cmd.uid(user.uid).gid(user.gid);
    }
    cmd.env("HOME", &user.home)
        .env("USER", &user.name)
        .env("LOGNAME", &user.name);
    cmd
}

fn lookup_user(name: &str) -> Result<Option<InvokingUser>> {
    let maybe = User::from_name(name).with_context(|| format!("failed to query user {name}"))?;
    Ok(maybe.map(|u| InvokingUser {
        name: u.name,
        home: u.dir,
        uid: u.uid.as_raw(),
        gid: u.gid.as_raw(),
    }))
}

fn try_logname() -> Option<String> {
    let out = Command::new("logname").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}
