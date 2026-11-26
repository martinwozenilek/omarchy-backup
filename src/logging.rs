use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use anyhow::{Context, Result};
use chrono::Local;
use nix::unistd::{Gid, Uid, chown, geteuid};

struct LoggerInner {
    file: File,
    echo_stdout: bool,
}

/// Thread-safe dual logger writing simultaneously to stdout and a user-owned log file.
#[derive(Clone)]
pub struct Logger {
    path: PathBuf,
    inner: Arc<Mutex<LoggerInner>>,
}

impl Logger {
    /// Create `log_dir` (assigning any newly created directory to `owner` if running as root)
    /// and open `backup-<stamp>.log`.
    pub fn new(log_dir: &Path, stamp: &str, owner: Option<(u32, u32)>) -> Result<Self> {
        create_dir_all_owned(log_dir, owner)?;
        let path = log_dir.join(format!("backup-{stamp}.log"));
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("failed to open log file {}", path.display()))?;

        if let Some((uid, gid)) = owner
            && geteuid().is_root()
        {
            chown(&path, Some(Uid::from_raw(uid)), Some(Gid::from_raw(gid)))
                .with_context(|| format!("failed to chown log file {}", path.display()))?;
        }

        Ok(Self {
            path,
            inner: Arc::new(Mutex::new(LoggerInner {
                file,
                echo_stdout: true,
            })),
        })
    }

    /// Create a logger that only writes to the file (useful for quiet unit tests).
    #[cfg(test)]
    pub fn new_silent(log_dir: &Path, stamp: &str) -> Result<Self> {
        let logger = Self::new(log_dir, stamp, None)?;
        if let Ok(mut guard) = logger.inner.lock() {
            guard.echo_stdout = false;
        }
        Ok(logger)
    }

    pub fn log_path(&self) -> &Path {
        &self.path
    }

    pub fn log(&self, msg: &str) {
        let line = format!("[{}] {msg}\n", Local::now().format("%T"));
        self.write_raw(line.as_bytes());
    }

    pub fn write_raw(&self, data: &[u8]) {
        if let Ok(mut guard) = self.inner.lock() {
            let _ = guard.file.write_all(data);
            let _ = guard.file.flush();
            if guard.echo_stdout {
                let mut stdout = io::stdout().lock();
                let _ = stdout.write_all(data);
                let _ = stdout.flush();
            }
        }
    }

    pub fn flush(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            let _ = guard.file.flush();
            if guard.echo_stdout {
                let _ = io::stdout().flush();
            }
        }
    }
}

/// Create `dir` and any missing ancestors, setting ownership to `owner` on every newly created
/// directory when running as root.
pub fn create_dir_all_owned(dir: &Path, owner: Option<(u32, u32)>) -> Result<()> {
    let mut missing = Vec::new();
    let mut cur = dir;
    while !cur.exists() {
        missing.push(cur.to_path_buf());
        match cur.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => cur = parent,
            _ => break,
        }
    }

    for path in missing.into_iter().rev() {
        fs::create_dir(&path)
            .with_context(|| format!("failed to create directory {}", path.display()))?;
        if let Some((uid, gid)) = owner
            && geteuid().is_root()
        {
            chown(&path, Some(Uid::from_raw(uid)), Some(Gid::from_raw(gid)))
                .with_context(|| format!("failed to chown directory {}", path.display()))?;
        }
    }
    Ok(())
}

/// Keep the newest `keep` `backup-*.log` files in `log_dir` and delete older ones.
pub fn rotate_logs(log_dir: &Path, keep: usize) -> Result<()> {
    let entries = match fs::read_dir(log_dir) {
        Ok(rd) => rd,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("failed to read log directory {}", log_dir.display()));
        }
    };

    let mut logs: Vec<(SystemTime, String, PathBuf)> = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("backup-") || !name.ends_with(".log") {
            continue;
        }
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        logs.push((mtime, name, path));
    }

    // Sort newest first (by mtime descending, then filename descending)
    logs.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));

    for (_, _, old_path) in logs.into_iter().skip(keep) {
        let _ = fs::remove_file(old_path);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;
    use tempfile::tempdir;

    #[test]
    fn logger_creates_nested_dirs_and_writes_lines() {
        let tmp = tempdir().unwrap();
        let nested = tmp.path().join("a/b/c");
        let logger = Logger::new_silent(&nested, "20260101-120000").unwrap();
        logger.log("hello world");
        logger.flush();

        let content = fs::read_to_string(logger.log_path()).unwrap();
        assert!(content.contains("hello world"));
    }

    #[test]
    fn rotate_logs_keeps_newest_and_ignores_unrelated_files() {
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join("notes.txt"), "keep").unwrap();

        for i in 1..=5 {
            let name = format!("backup-20260101-12000{i}.log");
            fs::write(tmp.path().join(name), format!("log {i}")).unwrap();
            thread::sleep(Duration::from_millis(15));
        }

        rotate_logs(tmp.path(), 2).unwrap();

        assert!(tmp.path().join("notes.txt").exists());
        assert!(tmp.path().join("backup-20260101-120005.log").exists());
        assert!(tmp.path().join("backup-20260101-120004.log").exists());
        assert!(!tmp.path().join("backup-20260101-120003.log").exists());
        assert!(!tmp.path().join("backup-20260101-120001.log").exists());
    }
}
