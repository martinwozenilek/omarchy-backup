use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use nix::sys::statvfs::statvfs;
use nix::unistd::sync;
use serde::Deserialize;

use crate::config::Settings;
use crate::logging::Logger;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetDrive {
    pub fs_dev: PathBuf,
    pub mount_point: PathBuf,
    pub fstype: String,
    pub options: String,
}

#[derive(Debug, Deserialize)]
struct FindmntOutput {
    #[serde(default)]
    filesystems: Vec<FindmntEntry>,
}

#[derive(Debug, Deserialize)]
struct FindmntEntry {
    source: Option<String>,
    target: Option<String>,
    fstype: Option<String>,
    options: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LsblkOutput {
    #[serde(default)]
    blockdevices: Vec<LsblkEntry>,
}

#[derive(Debug, Deserialize)]
struct LsblkEntry {
    pkname: Option<String>,
}

/// Parse `findmnt --json -l --nofsroot -o SOURCE,TARGET,FSTYPE,OPTIONS` output.
pub fn parse_findmnt_json(json: &str) -> Result<Option<TargetDrive>> {
    let parsed: FindmntOutput =
        serde_json::from_str(json).context("failed to parse findmnt JSON output")?;
    for entry in parsed.filesystems {
        if let (Some(source), Some(target), Some(fstype)) =
            (entry.source, entry.target, entry.fstype)
            && !source.is_empty()
            && !target.is_empty()
        {
            return Ok(Some(TargetDrive {
                fs_dev: PathBuf::from(source),
                mount_point: PathBuf::from(target),
                fstype,
                options: entry.options.unwrap_or_default(),
            }));
        }
    }
    Ok(None)
}

/// Discover the mounted backup drive by its btrfs filesystem label.
/// Never unlocks or mounts anything.
pub fn discover_target(settings: &Settings) -> Result<TargetDrive> {
    let label_arg = format!("LABEL={}", settings.fs_label);
    let out = Command::new("findmnt")
        .args([
            "--json",
            "-l",
            "--nofsroot",
            "-S",
            &label_arg,
            "-o",
            "SOURCE,TARGET,FSTYPE,OPTIONS",
        ])
        .output()
        .context("failed to run findmnt")?;

    if out.status.success() {
        let stdout = String::from_utf8_lossy(&out.stdout);
        if let Some(target) = parse_findmnt_json(&stdout)? {
            return Ok(target);
        }
    }

    bail!(
        "Backup drive (btrfs label {}) is not mounted. Plug it in and unlock it in the file manager, then retry.",
        settings.fs_label
    );
}

pub fn is_readonly_mount(options: &str) -> bool {
    options.split(',').any(|opt| opt == "ro")
}

pub fn available_bytes(path: &Path) -> Result<u64> {
    let stat = statvfs(path).with_context(|| format!("statvfs failed for {}", path.display()))?;
    #[allow(clippy::unnecessary_cast)]
    let avail = (stat.blocks_available() as u64).saturating_mul(stat.fragment_size() as u64);
    Ok(avail)
}

pub fn format_bytes_human(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    const TIB: f64 = GIB * 1024.0;
    let b = bytes as f64;
    if b >= TIB {
        format!("{:.1} TiB", b / TIB)
    } else if b >= GIB {
        format!("{:.1} GiB", b / GIB)
    } else if b >= MIB {
        format!("{:.1} MiB", b / MIB)
    } else if b >= KIB {
        format!("{:.1} KiB", b / KIB)
    } else {
        format!("{bytes} B")
    }
}

/// Validate marker file, filesystem type, read-write mount, and minimum free space.
pub fn validate_target(
    target: &TargetDrive,
    settings: &Settings,
    dry_run: bool,
    logger: &Logger,
) -> Result<u64> {
    let marker_path = target.mount_point.join(&settings.marker);
    if !marker_path.is_file() {
        bail!(
            "Marker {} missing on {}: wrong drive. Aborting.",
            settings.marker,
            target.mount_point.display()
        );
    }

    if target.fstype != "btrfs" {
        bail!("Target is not btrfs (found {}). Aborting.", target.fstype);
    }

    if is_readonly_mount(&target.options) {
        if dry_run {
            logger.log(&format!(
                "WARNING: {} is mounted read-only.",
                target.mount_point.display()
            ));
        } else {
            bail!(
                "Target {} is mounted read-only. Aborting.",
                target.mount_point.display()
            );
        }
    }

    let avail = available_bytes(&target.mount_point)?;
    let min_bytes = settings.min_free_gib.saturating_mul(1024 * 1024 * 1024);
    if avail < min_bytes {
        let avail_human = format_bytes_human(avail);
        if dry_run {
            logger.log(&format!(
                "WARNING: less than {} GiB free on {} ({avail_human} available).",
                settings.min_free_gib,
                target.mount_point.display()
            ));
        } else {
            bail!(
                "Less than {} GiB free on {} ({avail_human} available). Aborting.",
                settings.min_free_gib,
                target.mount_point.display()
            );
        }
    }

    Ok(avail)
}

/// Unmount, lock the LUKS volume, and power off the USB drive via `udisksctl`.
/// Skips `power-off` if `lock` fails.
pub fn eject_target(target: &TargetDrive, logger: &Logger) {
    sync();

    let (luks_part, disk) = match resolve_backing_devices(&target.fs_dev) {
        Some(pair) => pair,
        None => {
            logger.log(&format!(
                "WARNING: could not determine the USB device behind {}; drive left mounted.",
                target.fs_dev.display()
            ));
            return;
        }
    };

    let unmounted = Command::new("udisksctl")
        .args(["unmount", "-b"])
        .arg(&target.fs_dev)
        .stdout(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());

    if !unmounted {
        logger.log(&format!(
            "WARNING: could not unmount {} (busy?). Drive left mounted.",
            target.mount_point.display()
        ));
        return;
    }

    let locked = Command::new("udisksctl")
        .args(["lock", "-b"])
        .arg(&luks_part)
        .stdout(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());

    if !locked {
        logger.log(&format!(
            "WARNING: unmounted, but could not lock {}; skipping power-off.",
            luks_part.display()
        ));
        return;
    }

    let powered_off = Command::new("udisksctl")
        .args(["power-off", "-b"])
        .arg(format!("/dev/{disk}"))
        .stdout(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());

    if powered_off {
        logger.log("Drive ejected and powered off - safe to unplug.");
    } else {
        logger.log("Unmounted and locked, but power-off failed.");
    }
}

fn resolve_backing_devices(fs_dev: &Path) -> Option<(PathBuf, String)> {
    let real = fs::canonicalize(fs_dev).ok()?;
    let dev_name = real.file_name()?.to_str()?;
    let slaves_dir = Path::new("/sys/class/block").join(dev_name).join("slaves");
    let first_slave = fs::read_dir(&slaves_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .next()?;
    let slave_name = first_slave.file_name().to_str()?.to_string();
    let luks_part = PathBuf::from("/dev").join(&slave_name);

    let out = Command::new("lsblk")
        .args(["--json", "-n", "-o", "PKNAME"])
        .arg(&luks_part)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let parsed: LsblkOutput = serde_json::from_slice(&out.stdout).ok()?;
    let disk = parsed
        .blockdevices
        .into_iter()
        .find_map(|d| d.pkname)
        .filter(|s| !s.is_empty())?;

    Some((luks_part, disk))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn parse_findmnt_json_extracts_first_mount() {
        let json = r#"{
            "filesystems": [
                {
                    "source": "/dev/mapper/luks-1234",
                    "target": "/run/media/martin/omarchy-backup",
                    "fstype": "btrfs",
                    "options": "rw,nosuid,nodev,relatime,compress=zstd:3"
                }
            ]
        }"#;
        let t = parse_findmnt_json(json).unwrap().unwrap();
        assert_eq!(t.fs_dev, Path::new("/dev/mapper/luks-1234"));
        assert_eq!(t.mount_point, Path::new("/run/media/martin/omarchy-backup"));
        assert_eq!(t.fstype, "btrfs");
        assert!(!is_readonly_mount(&t.options));
    }

    #[test]
    fn parse_findmnt_json_handles_empty_and_spaces() {
        let empty = r#"{"filesystems": []}"#;
        assert!(parse_findmnt_json(empty).unwrap().is_none());

        let with_spaces = r#"{
            "filesystems": [
                {
                    "source": "/dev/mapper/luks-abc",
                    "target": "/run/media/martin/my backup",
                    "fstype": "btrfs",
                    "options": "ro,relatime"
                }
            ]
        }"#;
        let t = parse_findmnt_json(with_spaces).unwrap().unwrap();
        assert_eq!(t.mount_point, Path::new("/run/media/martin/my backup"));
        assert!(is_readonly_mount(&t.options));
    }

    #[test]
    fn is_readonly_mount_matches_exact_ro_token() {
        assert!(is_readonly_mount("ro"));
        assert!(is_readonly_mount("rw,ro,relatime"));
        assert!(!is_readonly_mount("rw,errors=remount-ro,relatime"));
        assert!(!is_readonly_mount("rw,relatime"));
    }

    #[test]
    fn validate_target_checks_marker_fstype_and_ro() {
        let tmp = tempdir().unwrap();
        let log_dir = tmp.path().join("logs");
        let logger = Logger::new_silent(&log_dir, "20260101-000000").unwrap();
        let mnt = tmp.path().join("usb");
        fs::create_dir(&mnt).unwrap();

        let settings = Settings {
            min_free_gib: 0,
            ..Default::default()
        };

        let target = TargetDrive {
            fs_dev: PathBuf::from("/dev/mapper/test"),
            mount_point: mnt.clone(),
            fstype: "btrfs".to_string(),
            options: "rw,relatime".to_string(),
        };

        // Missing marker must fail
        assert!(validate_target(&target, &settings, false, &logger).is_err());

        // Create marker, wrong fstype must fail
        fs::write(mnt.join(&settings.marker), "").unwrap();
        let wrong_fs = TargetDrive {
            fstype: "ext4".to_string(),
            ..target.clone()
        };
        assert!(validate_target(&wrong_fs, &settings, false, &logger).is_err());

        // Read-only fails on real run, warns on dry-run
        let ro_target = TargetDrive {
            options: "ro,relatime".to_string(),
            ..target.clone()
        };
        assert!(validate_target(&ro_target, &settings, false, &logger).is_err());
        assert!(validate_target(&ro_target, &settings, true, &logger).is_ok());

        // Valid target passes
        assert!(validate_target(&target, &settings, false, &logger).is_ok());
    }

    #[test]
    fn format_bytes_human_formats_units() {
        assert_eq!(format_bytes_human(512), "512 B");
        assert_eq!(format_bytes_human(2048), "2.0 KiB");
        assert_eq!(format_bytes_human(5 * 1024 * 1024 * 1024), "5.0 GiB");
    }
}
