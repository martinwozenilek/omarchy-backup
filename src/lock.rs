use std::fs::{File, OpenOptions};
use std::path::Path;

use anyhow::{Context, Result, bail};
use nix::errno::Errno;
use nix::fcntl::{Flock, FlockArg};

/// RAII exclusive lock guard. Unlocks and closes the file descriptor on drop.
/// `std::fs::File` opens with `O_CLOEXEC` on Linux so child processes never inherit it.
pub struct LockGuard {
    _flock: Flock<File>,
}

pub fn acquire_lock(path: &Path) -> Result<LockGuard> {
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("failed to open lock file {}", path.display()))?;

    match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
        Ok(flock) => Ok(LockGuard { _flock: flock }),
        Err((_, Errno::EWOULDBLOCK)) => bail!("Another backup is already running."),
        Err((_, err)) => Err(anyhow::Error::new(err))
            .with_context(|| format!("failed to lock {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn exclusive_lock_prevents_concurrent_acquisition() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("test.lock");

        let guard = acquire_lock(&lock_path).expect("first lock succeeds");
        let second = acquire_lock(&lock_path);
        assert!(second.is_err());
        assert!(
            second
                .err()
                .unwrap()
                .to_string()
                .contains("Another backup is already running")
        );

        drop(guard);
        let _third = acquire_lock(&lock_path).expect("re-acquire after drop succeeds");
    }
}
