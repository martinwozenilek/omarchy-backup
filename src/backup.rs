use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, anyhow};
use chrono::{Local, SecondsFormat};
use nix::sys::utsname::uname;
use nix::unistd::sync;
use tempfile::TempDir;

use crate::cli::BackupArgs;
use crate::config::{discover_config_path, load_config};
use crate::constants::{LOCK_FILE_PATH, LOG_STATE_REL_DIR};
use crate::lock::acquire_lock;
use crate::logging::{Logger, rotate_logs};
use crate::luks::backup_root_luks_header;
use crate::notify::{Urgency, notify};
use crate::packages::dump_package_lists;
use crate::privilege::{InvokingUser, ensure_root, resolve_invoking_user};
use crate::sync::{RsyncOutcome, build_rsync_args, run_rsync};
use crate::target::{
    TargetDrive, available_bytes, discover_target, eject_target, format_bytes_human,
    validate_target,
};

/// Options for executing a backup run (decoupled from `ensure_root` for integration testing).
pub struct BackupRunOptions {
    pub user: InvokingUser,
    pub config_path: PathBuf,
    pub lock_path: PathBuf,
    pub dry_run: bool,
    pub verbose: bool,
    pub eject: bool,
    pub interrupted: Arc<AtomicBool>,
}

/// Entrypoint for CLI backup invocation: elevates via `sudo`, resolves user/config, installs signal
/// handler, and executes the backup.
pub fn run_backup_cli(args: BackupArgs) -> i32 {
    if let Err(err) = ensure_root() {
        eprintln!("{err:#}");
        return 1;
    }

    let user = match resolve_invoking_user() {
        Ok(u) => u,
        Err(err) => {
            eprintln!("{err:#}");
            return 1;
        }
    };

    let config_path = match discover_config_path(args.config.as_deref(), &user.home) {
        Ok(p) => p,
        Err(err) => {
            eprintln!("{err:#}");
            return 1;
        }
    };

    let interrupted = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&interrupted);
    let _ = ctrlc::set_handler(move || {
        flag.store(true, Ordering::SeqCst);
    });

    execute_backup(BackupRunOptions {
        user,
        config_path,
        lock_path: PathBuf::from(LOCK_FILE_PATH),
        dry_run: args.dry_run,
        verbose: args.verbose,
        eject: args.eject,
        interrupted,
    })
}

/// Run the full backup sequence and return the process exit code.
pub fn execute_backup(opts: BackupRunOptions) -> i32 {
    // 1. Acquire exclusive lock BEFORE creating log file (avoids log spam on lock contention)
    let _lock = match acquire_lock(&opts.lock_path) {
        Ok(guard) => guard,
        Err(err) => {
            eprintln!("{err:#}");
            return 1;
        }
    };

    // 2. Initialize user-owned log file
    let stamp = Local::now().format("%Y%m%d-%H%M%S").to_string();
    let log_dir = opts.user.home.join(LOG_STATE_REL_DIR);
    let logger = match Logger::new(&log_dir, &stamp, Some((opts.user.uid, opts.user.gid))) {
        Ok(l) => l,
        Err(err) => {
            eprintln!("{err:#}");
            return 1;
        }
    };

    let mut discovered_target: Option<TargetDrive> = None;

    let outcome = run_backup_inner(&opts, &stamp, &log_dir, &logger, &mut discovered_target);

    let exit_code = match outcome {
        Ok(()) => 0,
        Err(RunFailure::ExitCode(code)) => code,
        Err(RunFailure::Other(err)) => {
            logger.log(&format!("{err:#}"));
            1
        }
    };

    if exit_code != 0 {
        notify(
            &opts.user,
            "Backup FAILED",
            &format!("See {}", logger.log_path().display()),
            Urgency::Critical,
        );
    } else if opts.eject
        && !opts.dry_run
        && let Some(target) = &discovered_target
    {
        eject_target(target, &logger);
    }

    logger.flush();
    exit_code
}

enum RunFailure {
    ExitCode(i32),
    Other(anyhow::Error),
}

impl From<anyhow::Error> for RunFailure {
    fn from(err: anyhow::Error) -> Self {
        Self::Other(err)
    }
}

fn run_backup_inner(
    opts: &BackupRunOptions,
    stamp: &str,
    log_dir: &Path,
    logger: &Logger,
    discovered_target: &mut Option<TargetDrive>,
) -> std::result::Result<(), RunFailure> {
    // Load & validate config
    let config = load_config(&opts.config_path, &opts.user.home).map_err(|err| {
        logger.log(&format!("{err:#}"));
        logger.log("Invalid config, aborting.");
        RunFailure::ExitCode(1)
    })?;

    // Discover & validate mounted USB drive
    let target = discover_target(&config.settings)?;
    validate_target(&target, &config.settings, opts.dry_run, logger)?;
    logger.log(&format!(
        "Using {} mounted at {}",
        target.fs_dev.display(),
        target.mount_point.display()
    ));
    *discovered_target = Some(target.clone());

    let sys = uname().context("uname failed")?;
    let host_name = sys.nodename().to_string_lossy().into_owned();
    let kernel_release = sys.release().to_string_lossy().into_owned();

    let dest = target
        .mount_point
        .join(&config.settings.dest_dir)
        .join(&host_name);
    let deleted = dest.join("_deleted").join(stamp);

    if !opts.dry_run {
        fs::create_dir_all(dest.join("packages"))
            .context("failed to create packages directory on target")?;
        fs::create_dir_all(dest.join("system"))
            .context("failed to create system directory on target")?;
    }

    // 1. Package lists
    if !opts.dry_run {
        check_interrupted(&opts.interrupted)?;
        dump_package_lists(&dest.join("packages"), &opts.user, logger)?;
    }

    // 2. LUKS header backup
    if !opts.dry_run {
        check_interrupted(&opts.interrupted)?;
        backup_root_luks_header(&dest.join("system"), logger);
    }

    // 3. Sync configured sources
    let work_dir = TempDir::new().context("failed to create temporary directory")?;

    for (idx, src) in config.sources.iter().enumerate() {
        check_interrupted(&opts.interrupted)?;

        if !src.path.exists() {
            if src.optional {
                logger.log(&format!("skip: {} (not present)", src.path.display()));
                continue;
            }
            return Err(RunFailure::Other(anyhow!(
                "Source {} does not exist. Aborting.",
                src.path.display()
            )));
        }
        if !src.path.is_dir() {
            return Err(RunFailure::Other(anyhow!(
                "Source {} is not a directory. Aborting.",
                src.path.display()
            )));
        }

        if !opts.dry_run {
            fs::create_dir_all(dest.join(&src.dest))
                .with_context(|| format!("failed to create destination {}", src.dest))?;
        }

        let exclude_file = if src.excludes.is_empty() {
            None
        } else {
            let p = work_dir.path().join(format!("excludes-{}", idx + 1));
            let content = format!("{}\n", src.excludes.join("\n"));
            fs::write(&p, content).context("failed to write rsync exclude file")?;
            Some(p)
        };

        let rsync_args = build_rsync_args(
            src,
            &dest,
            &deleted,
            exclude_file.as_deref(),
            opts.dry_run,
            opts.verbose,
        );
        let label = src.path.display().to_string();
        match run_rsync(&label, &rsync_args, logger, &opts.interrupted)? {
            RsyncOutcome::Ok | RsyncOutcome::VanishedFiles => {}
            RsyncOutcome::Failed(code) => return Err(RunFailure::ExitCode(code)),
            RsyncOutcome::Interrupted => return Err(RunFailure::ExitCode(130)),
        }
    }

    // 4. Finish (real runs only)
    if !opts.dry_run {
        prune_old_deleted(&dest.join("_deleted"), config.settings.keep_deleted_days);
        write_last_backup(&dest, &host_name, &kernel_release)?;
        sync();
        let _ = rotate_logs(log_dir, config.settings.keep_logs);
    }

    let free_human = available_bytes(&target.mount_point)
        .map(format_bytes_human)
        .unwrap_or_else(|_| "unknown".to_string());
    let suffix = if opts.dry_run { " (dry run)" } else { "" };
    logger.log(&format!(
        "Backup finished{suffix}. Free space: {free_human}"
    ));
    notify(
        &opts.user,
        "Backup finished",
        if opts.dry_run { "Dry run" } else { "OK" },
        Urgency::Normal,
    );

    Ok(())
}

fn check_interrupted(flag: &AtomicBool) -> std::result::Result<(), RunFailure> {
    if flag.load(Ordering::SeqCst) {
        Err(RunFailure::ExitCode(130))
    } else {
        Ok(())
    }
}

/// Remove direct child directories of `deleted_root` whose mtime is older than `keep_days`.
pub fn prune_old_deleted(deleted_root: &Path, keep_days: u64) {
    let Ok(entries) = fs::read_dir(deleted_root) else {
        return;
    };
    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_secs(keep_days.saturating_mul(86_400)))
        .unwrap_or(SystemTime::UNIX_EPOCH);

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let Ok(mtime) = meta.modified() else {
            continue;
        };
        if mtime < cutoff {
            let _ = fs::remove_dir_all(&path);
        }
    }
}

pub fn write_last_backup(dest: &Path, host_name: &str, kernel_release: &str) -> Result<()> {
    let now = Local::now().to_rfc3339_opts(SecondsFormat::Secs, false);
    let content = format!("date: {now}\nhost: {host_name}\nkernel: {kernel_release}\n");
    fs::write(dest.join("LAST_BACKUP"), content).context("failed to write LAST_BACKUP")
}
