use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Deserialize;

use crate::logging::Logger;

#[derive(Debug, Deserialize)]
struct FindmntOutput {
    #[serde(default)]
    filesystems: Vec<FindmntEntry>,
}

#[derive(Debug, Deserialize)]
struct FindmntEntry {
    source: Option<String>,
}

/// Extract the backing device path from `cryptsetup status <name>` output.
pub fn parse_cryptsetup_status_device(output: &str) -> Option<PathBuf> {
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("device:") {
            let dev = rest.trim();
            if !dev.is_empty() {
                return Some(PathBuf::from(dev));
            }
        }
    }
    None
}

/// Detect the root LUKS backing device dynamically from `/` (falling back to mapper `root`).
pub fn detect_root_luks_dev() -> Option<PathBuf> {
    if let Some(root_src) = find_root_mount_source()
        && let Ok(real) = fs::canonicalize(&root_src)
        && let Some(dev_name) = real.file_name().and_then(|n| n.to_str())
    {
        let dm_name_path = Path::new("/sys/class/block").join(dev_name).join("dm/name");
        if let Ok(dm_name) = fs::read_to_string(&dm_name_path) {
            let dm_name = dm_name.trim();
            if !dm_name.is_empty()
                && let Some(dev) = cryptsetup_status_device(dm_name)
            {
                return Some(dev);
            }
        }
    }

    cryptsetup_status_device("root")
}

/// Atomically back up the root LUKS header into `system_dir` (on the encrypted USB drive)
/// with mode `0600`. Logs warnings instead of aborting on failure.
pub fn backup_root_luks_header(system_dir: &Path, logger: &Logger) {
    let Some(root_luks_dev) = detect_root_luks_dev() else {
        logger.log("WARNING: could not detect root LUKS device, skipping header backup");
        return;
    };

    let Some(dev_base) = root_luks_dev.file_name().and_then(|n| n.to_str()) else {
        logger.log("WARNING: invalid root LUKS device path, skipping header backup");
        return;
    };

    let hdr_name = format!("luks-header-{dev_base}.img");
    let hdr = system_dir.join(&hdr_name);
    let hdr_tmp = system_dir.join(format!(".{hdr_name}.tmp"));

    let _ = fs::remove_file(&hdr_tmp);

    let status = Command::new("cryptsetup")
        .arg("luksHeaderBackup")
        .arg(&root_luks_dev)
        .arg("--header-backup-file")
        .arg(&hdr_tmp)
        .stdin(Stdio::null())
        .status();

    match status {
        Ok(s) if s.success() => {
            if let Err(err) = fs::set_permissions(&hdr_tmp, Permissions::from_mode(0o600)) {
                let _ = fs::remove_file(&hdr_tmp);
                logger.log(&format!(
                    "WARNING: failed to chmod LUKS header backup: {err}"
                ));
                return;
            }
            if let Err(err) = fs::rename(&hdr_tmp, &hdr) {
                let _ = fs::remove_file(&hdr_tmp);
                logger.log(&format!(
                    "WARNING: failed to finalize LUKS header backup: {err}"
                ));
            }
        }
        _ => {
            let _ = fs::remove_file(&hdr_tmp);
            logger.log("WARNING: LUKS header backup failed");
        }
    }
}

fn find_root_mount_source() -> Option<PathBuf> {
    let out = Command::new("findmnt")
        .args(["--json", "-l", "--nofsroot", "-M", "/", "-o", "SOURCE"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let parsed: FindmntOutput = serde_json::from_slice(&out.stdout).ok()?;
    parsed
        .filesystems
        .into_iter()
        .find_map(|e| e.source)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

fn cryptsetup_status_device(mapper_name: &str) -> Option<PathBuf> {
    let out = Command::new("cryptsetup")
        .args(["status", mapper_name])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_cryptsetup_status_device(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cryptsetup_status_output() {
        let sample = r#"/dev/mapper/root is active and is in use.
  type:    LUKS2
  cipher:  aes-xts-plain64
  keysize: 512 [bits]
  device:  /dev/sda2
  sector size:  512 [bytes]
"#;
        assert_eq!(
            parse_cryptsetup_status_device(sample),
            Some(PathBuf::from("/dev/sda2"))
        );
        assert_eq!(parse_cryptsetup_status_device("not active"), None);
    }
}
