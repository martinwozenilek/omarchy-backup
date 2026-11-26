use std::ffi::OsString;
use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use omarchy_backup::backup::{BackupRunOptions, execute_backup};
use omarchy_backup::privilege::InvokingUser;
use tempfile::TempDir;

static PATH_LOCK: Mutex<()> = Mutex::new(());

struct PathGuard {
    old_path: Option<OsString>,
}

impl PathGuard {
    fn prepend(bin_dir: &Path) -> Self {
        let old_path = std::env::var_os("PATH");
        let mut new_path = OsString::from(bin_dir.as_os_str());
        if let Some(ref old) = old_path {
            new_path.push(":");
            new_path.push(old);
        }
        // SAFETY: serialized via PATH_LOCK in this test binary.
        unsafe {
            std::env::set_var("PATH", new_path);
        }
        Self { old_path }
    }
}

impl Drop for PathGuard {
    fn drop(&mut self) {
        // SAFETY: serialized via PATH_LOCK in this test binary.
        unsafe {
            match &self.old_path {
                Some(old) => std::env::set_var("PATH", old),
                None => std::env::remove_var("PATH"),
            }
        }
    }
}

fn write_script(dir: &Path, name: &str, body: &str) {
    let path = dir.join(name);
    fs::write(&path, format!("#!/usr/bin/env bash\n{body}\n")).unwrap();
    fs::set_permissions(&path, Permissions::from_mode(0o755)).unwrap();
}

struct MockEnv {
    _tmp: TempDir,
    bin_dir: PathBuf,
    usb_dir: PathBuf,
    home_dir: PathBuf,
    trace_file: PathBuf,
    lock_file: PathBuf,
    cfg_file: PathBuf,
}

impl MockEnv {
    fn new(rsync_behavior: &str) -> Self {
        let tmp = TempDir::new().unwrap();
        let bin_dir = tmp.path().join("bin");
        let usb_dir = tmp.path().join("usb");
        let home_dir = tmp.path().join("home");
        let src_etc = tmp.path().join("etc");
        let src_boot = tmp.path().join("boot");
        let trace_file = tmp.path().join("trace.log");
        let lock_file = tmp.path().join("backup.lock");
        let cfg_file = tmp.path().join("omarchy-backup.toml");

        for d in [&bin_dir, &usb_dir, &home_dir, &src_etc, &src_boot] {
            fs::create_dir_all(d).unwrap();
        }
        fs::write(usb_dir.join(".omarchy-backup-target"), "").unwrap();

        let cfg_content = format!(
            r#"[settings]
min_free_gib = 0
keep_deleted_days = 30
keep_logs = 2

[[source]]
path = "{etc}"
dest = "system/etc"

[[source]]
path = "{boot}"
dest = "system/boot"
vfat = true
one_file_system = false

[[source]]
path = "/nonexistent/optional/dir"
dest = "system/optional"
optional = true

[[source]]
path = "~"
dest = "home"
excludes = ["/.cache/", "*.tmp"]
"#,
            etc = src_etc.display(),
            boot = src_boot.display()
        );
        fs::write(&cfg_file, cfg_content).unwrap();

        let usb_str = usb_dir.display().to_string();
        let trace_str = trace_file.display().to_string();

        write_script(
            &bin_dir,
            "findmnt",
            &format!(
                r#"if [[ "$*" == *"LABEL=omarchy-backup"* ]]; then
  printf '{{"filesystems":[{{"source":"/dev/mapper/mock-luks","target":"{usb_str}","fstype":"btrfs","options":"rw,relatime"}}]}}\n'
  exit 0
fi
exit 1"#
            ),
        );

        write_script(
            &bin_dir,
            "rsync",
            &format!(
                r#"printf 'rsync %s\n' "$*" >> "{trace_str}"
{rsync_behavior}"#
            ),
        );

        write_script(
            &bin_dir,
            "pacman",
            &format!(
                r#"printf 'pacman %s\n' "$*" >> "{trace_str}"
case "$1" in
  -Qqen) printf 'base\nlinux\n' ;;
  -Qqem) exit 1 ;;
  -Qqe)  printf 'base\nlinux\n' ;;
  -Qqd)  printf 'glibc\n' ;;
  -Q)    printf 'base 3-2\n' ;;
  -Qii)  printf 'Name            : filesystem\nBackup Files    : /etc/fstab [modified]\n                  /etc/hosts [unmodified]\n' ;;
esac"#
            ),
        );

        write_script(
            &bin_dir,
            "cryptsetup",
            &format!(
                r#"printf 'cryptsetup %s\n' "$*" >> "{trace_str}"
if [[ "$1" == "status" && "$2" == "root" ]]; then
  printf '/dev/mapper/root is active.\n  device:  /dev/sda2\n'
  exit 0
elif [[ "$1" == "luksHeaderBackup" ]]; then
  # arg 4 is --header-backup-file, arg 5 is path (actually $3 is --header-backup-file, $4 is path)
  printf 'MOCK_LUKS_HEADER' > "$4"
  exit 0
fi
exit 1"#
            ),
        );

        write_script(
            &bin_dir,
            "omarchy",
            &format!(r#"printf 'omarchy %s\n' "$*" >> "{trace_str}"; echo "1.2.3""#),
        );

        write_script(
            &bin_dir,
            "systemctl",
            &format!(
                r#"printf 'systemctl %s\n' "$*" >> "{trace_str}"; echo "sshd.service enabled""#
            ),
        );

        write_script(
            &bin_dir,
            "udisksctl",
            &format!(r#"printf 'udisksctl %s\n' "$*" >> "{trace_str}""#),
        );

        write_script(
            &bin_dir,
            "notify-send",
            &format!(r#"printf 'notify-send %s\n' "$*" >> "{trace_str}""#),
        );

        Self {
            _tmp: tmp,
            bin_dir,
            usb_dir,
            home_dir,
            trace_file,
            lock_file,
            cfg_file,
        }
    }

    fn user(&self) -> InvokingUser {
        InvokingUser {
            name: "testuser".to_string(),
            home: self.home_dir.clone(),
            uid: 1000,
            gid: 1000,
        }
    }
}

#[test]
fn dry_run_makes_zero_writes_to_usb_and_skips_eject() {
    let _guard = PATH_LOCK.lock().unwrap();
    let env = MockEnv::new("exit 0");
    let _path_guard = PathGuard::prepend(&env.bin_dir);

    let rc = execute_backup(BackupRunOptions {
        user: env.user(),
        config_path: env.cfg_file.clone(),
        lock_path: env.lock_file.clone(),
        dry_run: true,
        verbose: false,
        eject: true,
        interrupted: Arc::new(AtomicBool::new(false)),
    });
    assert_eq!(rc, 0);

    // USB mount must remain completely untouched (only marker file)
    let usb_entries: Vec<String> = fs::read_dir(&env.usb_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(usb_entries, vec![".omarchy-backup-target"]);

    let trace = fs::read_to_string(&env.trace_file).unwrap();
    assert!(!trace.contains("pacman "));
    assert!(!trace.contains("cryptsetup "));
    assert!(!trace.contains("udisksctl "));
    assert!(trace.contains("--dry-run --itemize-changes"));
    assert!(trace.contains("-rt --modify-window=2"));
    assert!(!trace.contains("--backup-dir="));
    assert!(trace.contains("notify-send -a omarchy-backup -u normal -- Backup finished Dry run"));
}

#[test]
fn real_run_dumps_metadata_handles_exit_24_and_prunes_deleted() {
    let _guard = PATH_LOCK.lock().unwrap();
    // Return exit 24 on the /etc rsync to verify exit 24 is treated as success
    let env = MockEnv::new(
        r#"if [[ "$*" == *"/etc/"* ]]; then
  echo "file has vanished" >&2
  exit 24
fi
exit 0"#,
    );
    let _path_guard = PathGuard::prepend(&env.bin_dir);

    // Pre-create an old _deleted entry (>30 days old) and a fresh one
    let host = nix::sys::utsname::uname()
        .unwrap()
        .nodename()
        .to_string_lossy()
        .into_owned();
    let deleted_root = env
        .usb_dir
        .join("omarchy-backup")
        .join(&host)
        .join("_deleted");
    let old_dir = deleted_root.join("20200101-000000");
    let fresh_dir = deleted_root.join("20990101-000000");
    fs::create_dir_all(&old_dir).unwrap();
    fs::create_dir_all(&fresh_dir).unwrap();
    let _ = Command::new("touch")
        .args(["-d", "60 days ago"])
        .arg(&old_dir)
        .status();

    let rc = execute_backup(BackupRunOptions {
        user: env.user(),
        config_path: env.cfg_file.clone(),
        lock_path: env.lock_file.clone(),
        dry_run: false,
        verbose: true,
        eject: false,
        interrupted: Arc::new(AtomicBool::new(false)),
    });
    assert_eq!(rc, 0);

    let host_dir = env.usb_dir.join("omarchy-backup").join(&host);
    assert_eq!(
        fs::read_to_string(host_dir.join("packages/pacman-native.txt")).unwrap(),
        "base\nlinux\n"
    );
    assert_eq!(
        fs::read_to_string(host_dir.join("packages/pacman-modified-files.txt")).unwrap(),
        "filesystem\t/etc/fstab\n"
    );
    assert_eq!(
        fs::read_to_string(host_dir.join("packages/omarchy-version.txt")).unwrap(),
        "1.2.3\n"
    );

    // LUKS header exists with mode 0600 and no .tmp leftover
    let hdr = host_dir.join("system/luks-header-sda2.img");
    assert_eq!(fs::read_to_string(&hdr).unwrap(), "MOCK_LUKS_HEADER");
    let mode = fs::metadata(&hdr).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert!(!host_dir.join("system/.luks-header-sda2.img.tmp").exists());

    // LAST_BACKUP written
    let last_backup = fs::read_to_string(host_dir.join("LAST_BACKUP")).unwrap();
    assert!(last_backup.contains("date: "));
    assert!(last_backup.contains(&format!("host: {host}")));
    assert!(last_backup.contains("kernel: "));

    // Old _deleted pruned, fresh kept
    assert!(!old_dir.exists());
    assert!(fresh_dir.exists());

    // Verify rsync invocations
    let trace = fs::read_to_string(&env.trace_file).unwrap();
    let rsync_lines: Vec<&str> = trace.lines().filter(|l| l.starts_with("rsync ")).collect();
    assert_eq!(rsync_lines.len(), 3);
    assert!(rsync_lines[0].contains("--backup-dir="));
    assert!(rsync_lines[0].contains(" -x "));
    assert!(rsync_lines[1].contains("-rt --modify-window=2"));
    assert!(!rsync_lines[1].contains("--backup-dir="));
    assert!(!rsync_lines[1].contains(" -x "));
    assert!(rsync_lines[2].contains("--exclude-from="));
}

#[test]
fn rsync_failure_propagates_code_and_sends_critical_notification() {
    let _guard = PATH_LOCK.lock().unwrap();
    let env = MockEnv::new("exit 23");
    let _path_guard = PathGuard::prepend(&env.bin_dir);

    let rc = execute_backup(BackupRunOptions {
        user: env.user(),
        config_path: env.cfg_file.clone(),
        lock_path: env.lock_file.clone(),
        dry_run: false,
        verbose: false,
        eject: true,
        interrupted: Arc::new(AtomicBool::new(false)),
    });
    assert_eq!(rc, 23);

    let trace = fs::read_to_string(&env.trace_file).unwrap();
    assert!(trace.contains("notify-send -a omarchy-backup -u critical -- Backup FAILED"));
    assert!(!trace.contains("udisksctl "));
}
