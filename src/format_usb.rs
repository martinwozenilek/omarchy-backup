use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, Write};
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::cli::FormatUsbArgs;
use crate::config::load_settings_for_format;
use crate::constants::{FORMAT_MAPPER_NAME, FORMAT_TEMP_MOUNT};
use crate::privilege::{ensure_root, resolve_home_best_effort};

#[derive(Debug, Deserialize)]
struct LsblkOutput {
    #[serde(default)]
    blockdevices: Vec<LsblkNode>,
}

#[derive(Debug, Deserialize)]
struct LsblkNode {
    name: String,
    #[serde(rename = "type", default)]
    dev_type: Option<String>,
    #[serde(default)]
    tran: Option<String>,
    #[serde(default)]
    pkname: Option<String>,
    #[serde(default)]
    children: Vec<LsblkNode>,
}

#[derive(Debug, Deserialize)]
struct FindmntOutput {
    #[serde(default)]
    filesystems: Vec<FindmntEntry>,
}

#[derive(Debug, Deserialize)]
struct FindmntEntry {
    source: Option<String>,
}

/// RAII guard that unmounts the temporary mount point and closes the LUKS mapper on failure.
struct FormatGuard {
    mount_point: PathBuf,
    mapper_name: String,
    active: bool,
}

impl FormatGuard {
    fn new(mount_point: PathBuf, mapper_name: String) -> Self {
        Self {
            mount_point,
            mapper_name,
            active: true,
        }
    }

    fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for FormatGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let _ = Command::new("umount")
            .arg(&self.mount_point)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if self.mount_point.is_dir() {
            let _ = fs::remove_dir(&self.mount_point);
        }
        let mapper_dev = Path::new("/dev/mapper").join(&self.mapper_name);
        if mapper_dev.exists() {
            let _ = Command::new("cryptsetup")
                .args(["close", &self.mapper_name])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

pub fn run_format_usb(args: FormatUsbArgs) -> Result<()> {
    ensure_root()?;

    let user_home = resolve_home_best_effort();
    let (settings, cfg_used) = load_settings_for_format(args.config.as_deref(), &user_home)?;
    if let Some(cfg_path) = cfg_used {
        println!(
            "Using labels from {}: LUKS={}, btrfs={}, marker={}",
            cfg_path.display(),
            settings.luks_label,
            settings.fs_label,
            settings.marker
        );
    }

    let meta = fs::metadata(&args.device)
        .with_context(|| format!("{} is not accessible", args.device.display()))?;
    if !meta.file_type().is_block_device() {
        bail!("{} is not a block device", args.device.display());
    }

    let disk = fs::canonicalize(&args.device)
        .with_context(|| format!("failed to canonicalize {}", args.device.display()))?;
    let disk_name = disk
        .file_name()
        .and_then(|n| n.to_str())
        .context("invalid disk device path")?
        .to_string();

    // Verify whole disk and USB transport
    let disk_info_out = Command::new("lsblk")
        .args(["--json", "-d", "-o", "NAME,TYPE,TRAN"])
        .arg(&disk)
        .output()
        .context("failed to run lsblk")?;
    if !disk_info_out.status.success() {
        bail!("lsblk failed for {}", disk.display());
    }
    validate_disk_is_usb_whole_disk(&String::from_utf8_lossy(&disk_info_out.stdout), &disk)?;

    // Refuse disks holding /, /boot, or /home
    for protected_mount in ["/", "/boot", "/home"] {
        if let Some(src) = mount_source(protected_mount)
            && let Ok(out) = Command::new("lsblk")
                .args(["--json", "-s", "-o", "NAME,PKNAME"])
                .arg(&src)
                .output()
            && out.status.success()
        {
            let backing = collect_device_names_from_lsblk(&String::from_utf8_lossy(&out.stdout))?;
            if backing.contains(&disk_name) {
                bail!("Refusing: {} holds {protected_mount}", disk.display());
            }
        }
    }

    // Show target disk layout and ask for explicit confirmation
    let _ = Command::new("lsblk")
        .args(["-o", "NAME,SIZE,FSTYPE,LABEL,MOUNTPOINT"])
        .arg(&disk)
        .status();

    print!(
        "ERASE EVERYTHING on {}? Type the device path to confirm: ",
        disk.display()
    );
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer)?;
    if answer.trim() != disk.to_string_lossy() {
        bail!("Aborted.");
    }

    let mnt = PathBuf::from(FORMAT_TEMP_MOUNT);
    let mapper = FORMAT_MAPPER_NAME.to_string();
    let mut guard = FormatGuard::new(mnt.clone(), mapper.clone());

    // Unmount existing child partitions/mappers (deepest first) and close any open crypt mappings
    unmount_and_close_existing(&disk)?;

    run_checked("wipefs", &["-a", &disk.to_string_lossy()])?;
    run_checked(
        "parted",
        &[
            "-s",
            &disk.to_string_lossy(),
            "mklabel",
            "gpt",
            "mkpart",
            &settings.luks_label,
            "1MiB",
            "100%",
        ],
    )?;
    run_checked("partprobe", &[&disk.to_string_lossy()])?;
    run_checked("udevadm", &["settle"])?;

    let part = locate_first_partition(&disk)?;
    let part_meta = fs::metadata(&part)
        .with_context(|| format!("newly created partition {} not found", part.display()))?;
    if !part_meta.file_type().is_block_device() {
        bail!(
            "Failed to locate newly created partition on {}",
            disk.display()
        );
    }

    // Interactive LUKS2 format (inherits TTY for passphrase prompt)
    run_interactive(
        "cryptsetup",
        &[
            "luksFormat",
            "--type",
            "luks2",
            "--label",
            &settings.luks_label,
            &part.to_string_lossy(),
        ],
    )?;
    run_checked("udevadm", &["settle"])?;

    // Close any udisks auto-unlocked holders on the fresh partition
    close_sysfs_holders(&part);

    run_interactive("cryptsetup", &["open", &part.to_string_lossy(), &mapper])?;

    let mapper_dev = format!("/dev/mapper/{mapper}");
    run_checked("mkfs.btrfs", &["-L", &settings.fs_label, &mapper_dev])?;

    fs::create_dir_all(&mnt)
        .with_context(|| format!("failed to create mount point {}", mnt.display()))?;
    run_checked(
        "mount",
        &[
            "-o",
            "noatime,compress=zstd:3",
            &mapper_dev,
            &mnt.to_string_lossy(),
        ],
    )?;

    let marker_path = mnt.join(&settings.marker);
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&marker_path)
        .with_context(|| format!("failed to create marker {}", marker_path.display()))?;

    run_checked("umount", &[&mnt.to_string_lossy()])?;
    fs::remove_dir(&mnt)
        .with_context(|| format!("failed to remove mount point {}", mnt.display()))?;
    run_checked("cryptsetup", &["close", &mapper])?;
    guard.disarm();

    println!(
        "Done. {} is ready. Unplug and replug the drive (or unlock it in the file manager), then run ./target/release/omarchy-backup --dry-run.",
        part.display()
    );
    Ok(())
}

pub fn validate_disk_is_usb_whole_disk(lsblk_json: &str, disk: &Path) -> Result<()> {
    let parsed: LsblkOutput =
        serde_json::from_str(lsblk_json).context("failed to parse lsblk JSON")?;
    let node = parsed
        .blockdevices
        .first()
        .with_context(|| format!("lsblk returned no info for {}", disk.display()))?;

    if node.dev_type.as_deref() != Some("disk") {
        bail!(
            "Refusing: {} is not a whole disk (pass e.g. /dev/sdX, not a partition)",
            disk.display()
        );
    }
    if node.tran.as_deref() != Some("usb") {
        bail!("Refusing: {} is not a USB disk", disk.display());
    }
    Ok(())
}

pub fn collect_device_names_from_lsblk(lsblk_json: &str) -> Result<HashSet<String>> {
    let parsed: LsblkOutput =
        serde_json::from_str(lsblk_json).context("failed to parse lsblk -s JSON")?;
    let mut names = HashSet::new();
    fn visit(node: &LsblkNode, out: &mut HashSet<String>) {
        out.insert(node.name.clone());
        if let Some(pk) = &node.pkname
            && !pk.is_empty()
        {
            out.insert(pk.clone());
        }
        for child in &node.children {
            visit(child, out);
        }
    }
    for dev in &parsed.blockdevices {
        visit(dev, &mut names);
    }
    Ok(names)
}

fn mount_source(mount_point: &str) -> Option<String> {
    let out = Command::new("findmnt")
        .args([
            "--json",
            "-l",
            "--nofsroot",
            "-M",
            mount_point,
            "-o",
            "SOURCE",
        ])
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
}

fn unmount_and_close_existing(disk: &Path) -> Result<()> {
    let out = Command::new("lsblk")
        .args(["--json", "-p", "-o", "NAME,TYPE"])
        .arg(disk)
        .output()
        .context("failed to inspect existing disk children")?;
    if !out.status.success() {
        return Ok(());
    }
    let parsed: LsblkOutput =
        serde_json::from_slice(&out.stdout).context("failed to parse lsblk tree JSON")?;

    let mut children_deepest_first: Vec<String> = Vec::new();
    let mut crypt_devices: Vec<String> = Vec::new();

    fn walk(node: &LsblkNode, is_root: bool, deepest: &mut Vec<String>, crypts: &mut Vec<String>) {
        for child in &node.children {
            walk(child, false, deepest, crypts);
        }
        if !is_root {
            deepest.push(node.name.clone());
            if node.dev_type.as_deref() == Some("crypt") {
                crypts.push(node.name.clone());
            }
        }
    }

    for dev in &parsed.blockdevices {
        walk(dev, true, &mut children_deepest_first, &mut crypt_devices);
    }

    for dev in &children_deepest_first {
        let _ = Command::new("umount")
            .arg(dev)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    for dm in &crypt_devices {
        let _ = Command::new("cryptsetup")
            .args(["close", dm])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    Ok(())
}

fn locate_first_partition(disk: &Path) -> Result<PathBuf> {
    let out = Command::new("lsblk")
        .args(["--json", "-p", "-o", "NAME,TYPE"])
        .arg(disk)
        .output()
        .context("failed to run lsblk after partitioning")?;
    if !out.status.success() {
        bail!(
            "lsblk failed while locating partition on {}",
            disk.display()
        );
    }
    let parsed: LsblkOutput =
        serde_json::from_slice(&out.stdout).context("failed to parse lsblk partition JSON")?;

    for dev in &parsed.blockdevices {
        for child in &dev.children {
            if child.dev_type.as_deref() == Some("part") {
                return Ok(PathBuf::from(&child.name));
            }
        }
    }
    bail!(
        "Failed to locate newly created partition on {}",
        disk.display()
    );
}

fn close_sysfs_holders(part: &Path) {
    let Some(part_name) = part.file_name().and_then(|n| n.to_str()) else {
        return;
    };
    let holders_dir = Path::new("/sys/class/block")
        .join(part_name)
        .join("holders");
    let Ok(entries) = fs::read_dir(&holders_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let hpath = entry.path();
        if let Some(hname) = entry.file_name().to_str() {
            let _ = Command::new("umount")
                .arg(format!("/dev/{hname}"))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        if let Ok(dm_name) = fs::read_to_string(hpath.join("dm/name")) {
            let dm_name = dm_name.trim();
            if !dm_name.is_empty() {
                let _ = Command::new("cryptsetup")
                    .args(["close", dm_name])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
    }
}

fn run_checked(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("failed to run {program}"))?;
    if !status.success() {
        bail!("{program} {} failed ({status})", args.join(" "));
    }
    Ok(())
}

fn run_interactive(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("failed to run {program}"))?;
    if !status.success() {
        bail!("{program} {} failed ({status})", args.join(" "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_usb_whole_disk_and_rejects_non_usb_or_partition() {
        let usb_disk = r#"{"blockdevices":[{"name":"sdb","type":"disk","tran":"usb"}]}"#;
        assert!(validate_disk_is_usb_whole_disk(usb_disk, Path::new("/dev/sdb")).is_ok());

        let nvme_disk = r#"{"blockdevices":[{"name":"nvme0n1","type":"disk","tran":"nvme"}]}"#;
        assert!(validate_disk_is_usb_whole_disk(nvme_disk, Path::new("/dev/nvme0n1")).is_err());

        let part = r#"{"blockdevices":[{"name":"sdb1","type":"part","tran":null}]}"#;
        assert!(validate_disk_is_usb_whole_disk(part, Path::new("/dev/sdb1")).is_err());
    }

    #[test]
    fn collects_nested_backing_disk_names_from_lsblk_inverse_tree() {
        let tree = r#"{
            "blockdevices": [
                {
                    "name": "root",
                    "pkname": "sda2",
                    "children": [
                        {
                            "name": "sda2",
                            "pkname": "sda",
                            "children": [
                                {"name": "sda", "pkname": null}
                            ]
                        }
                    ]
                }
            ]
        }"#;
        let names = collect_device_names_from_lsblk(tree).unwrap();
        assert!(names.contains("root"));
        assert!(names.contains("sda2"));
        assert!(names.contains("sda"));
        assert!(!names.contains("sdb"));
    }
}
