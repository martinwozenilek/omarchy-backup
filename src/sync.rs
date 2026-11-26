use std::ffi::OsString;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::config::Source;
use crate::logging::Logger;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RsyncOutcome {
    Ok,
    VanishedFiles,
    Failed(i32),
    Interrupted,
}

/// Build the exact `rsync` argument vector for a configured source.
pub fn build_rsync_args(
    source: &Source,
    host_dest: &Path,
    deleted_root: &Path,
    exclude_file: Option<&Path>,
    dry_run: bool,
    verbose: bool,
) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::new();

    if source.vfat {
        for flag in [
            "-rt",
            "--modify-window=2",
            "--delete",
            "--delete-excluded",
            "--human-readable",
        ] {
            args.push(OsString::from(flag));
        }
    } else {
        for flag in [
            "-aAXH",
            "--numeric-ids",
            "--delete",
            "--delete-excluded",
            "--human-readable",
        ] {
            args.push(OsString::from(flag));
        }
    }

    if dry_run {
        args.push(OsString::from("--dry-run"));
        args.push(OsString::from("--itemize-changes"));
    }
    if verbose {
        args.push(OsString::from("-v"));
    }

    if !source.vfat && !dry_run {
        args.push(OsString::from("--backup"));
        let mut backup_dir = OsString::from("--backup-dir=");
        backup_dir.push(deleted_root.join(&source.dest).as_os_str());
        args.push(backup_dir);
    }

    if source.one_file_system {
        args.push(OsString::from("-x"));
    }

    if let Some(xfile) = exclude_file {
        let mut arg = OsString::from("--exclude-from=");
        arg.push(xfile.as_os_str());
        args.push(arg);
    }

    args.push(with_trailing_slash(&source.path));
    args.push(with_trailing_slash(&host_dest.join(&source.dest)));
    args
}

/// Run `rsync`, streaming stdout and stderr live to `logger`.
/// Exit codes 0 and 24 (vanished source files) are treated as success.
pub fn run_rsync(
    label: &str,
    args: &[OsString],
    logger: &Logger,
    interrupted: &AtomicBool,
) -> Result<RsyncOutcome> {
    logger.log(&format!("rsync: {label}"));

    let mut child = Command::new("rsync")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn rsync for {label}"))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let logger_out = logger.clone();
    let t_out = thread::spawn(move || {
        if let Some(mut pipe) = stdout {
            stream_to_logger(&mut pipe, &logger_out);
        }
    });

    let logger_err = logger.clone();
    let t_err = thread::spawn(move || {
        if let Some(mut pipe) = stderr {
            stream_to_logger(&mut pipe, &logger_err);
        }
    });

    let mut was_interrupted = false;
    let status = loop {
        if interrupted.load(Ordering::SeqCst) {
            was_interrupted = true;
            let _ = child.kill();
            break child.wait().context("failed waiting on killed rsync")?;
        }
        match child.try_wait().context("failed polling rsync")? {
            Some(status) => break status,
            None => thread::sleep(Duration::from_millis(50)),
        }
    };

    let _ = t_out.join();
    let _ = t_err.join();

    if was_interrupted {
        logger.log(&format!("rsync interrupted for {label}"));
        return Ok(RsyncOutcome::Interrupted);
    }

    match status.code() {
        Some(0) => Ok(RsyncOutcome::Ok),
        Some(24) => {
            logger.log(&format!(
                "rsync: {label} (some source files vanished during transfer, exit 24)"
            ));
            Ok(RsyncOutcome::VanishedFiles)
        }
        Some(code) => {
            logger.log(&format!("rsync failed for {label} (exit {code})"));
            Ok(RsyncOutcome::Failed(code))
        }
        None => {
            logger.log(&format!("rsync terminated by signal for {label}"));
            Ok(RsyncOutcome::Failed(1))
        }
    }
}

fn stream_to_logger<R: Read>(reader: &mut R, logger: &Logger) {
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => logger.write_raw(&buf[..n]),
        }
    }
}

fn with_trailing_slash(path: &Path) -> OsString {
    let mut s = path.as_os_str().to_os_string();
    if !path.to_string_lossy().ends_with('/') {
        s.push("/");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn as_strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn builds_standard_real_run_args() {
        let src = Source {
            path: PathBuf::from("/home/user"),
            dest: "home".to_string(),
            optional: false,
            vfat: false,
            one_file_system: true,
            excludes: vec!["/.cache/".to_string()],
        };
        let args = as_strings(&build_rsync_args(
            &src,
            Path::new("/mnt/usb/omarchy-backup/host"),
            Path::new("/mnt/usb/omarchy-backup/host/_deleted/20260101-120000"),
            Some(Path::new("/tmp/ex-1")),
            false,
            true,
        ));
        assert!(args.contains(&"-aAXH".to_string()));
        assert!(args.contains(&"--numeric-ids".to_string()));
        assert!(args.contains(&"--backup".to_string()));
        assert!(args.contains(
            &"--backup-dir=/mnt/usb/omarchy-backup/host/_deleted/20260101-120000/home".to_string()
        ));
        assert!(args.contains(&"-x".to_string()));
        assert!(args.contains(&"-v".to_string()));
        assert!(args.contains(&"--exclude-from=/tmp/ex-1".to_string()));
        assert_eq!(args[args.len() - 2], "/home/user/");
        assert_eq!(args[args.len() - 1], "/mnt/usb/omarchy-backup/host/home/");
    }

    #[test]
    fn builds_vfat_dry_run_args_without_acls_or_backup_dir() {
        let src = Source {
            path: PathBuf::from("/boot"),
            dest: "system/boot".to_string(),
            optional: false,
            vfat: true,
            one_file_system: false,
            excludes: vec![],
        };
        let args = as_strings(&build_rsync_args(
            &src,
            Path::new("/mnt/usb/omarchy-backup/host"),
            Path::new("/mnt/usb/omarchy-backup/host/_deleted/20260101-120000"),
            None,
            true,
            false,
        ));
        assert!(args.contains(&"-rt".to_string()));
        assert!(args.contains(&"--modify-window=2".to_string()));
        assert!(args.contains(&"--delete-excluded".to_string()));
        assert!(args.contains(&"--dry-run".to_string()));
        assert!(args.contains(&"--itemize-changes".to_string()));
        assert!(!args.contains(&"-aAXH".to_string()));
        assert!(!args.contains(&"--backup".to_string()));
        assert!(!args.contains(&"-x".to_string()));
    }
}
