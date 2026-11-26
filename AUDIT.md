# Code Audit Report

Review of `omarchy-backup.sh`, `format-usb.sh`, `omarchy-backup.toml`, `RESTORE.md`, `README.md`, and `CLAUDE.md`.

---

## 1. Bugs & Glitches

### Critical / Broken Behavior

1. **Btrfs label mismatch between `format-usb.sh` and `omarchy-backup.sh`**
   - **Location:** `format-usb.sh` (`mkfs.btrfs -L "$LABEL"`) vs `omarchy-backup.sh` (`FS_LABEL="omarchy-backup"`)
   - **Impact:** `format-usb.sh` created the btrfs filesystem with uppercase label `OMARCHY-BACKUP`, while `omarchy-backup.sh` searches for lowercase `omarchy-backup` via case-sensitive `findmnt -S LABEL=...`. A freshly formatted drive could not be found by the backup script.
   - **Fix:** Separate `LUKS_LABEL="OMARCHY-BACKUP"` and `FS_LABEL="omarchy-backup"` in `format-usb.sh` and pass `-L "$FS_LABEL"` to `mkfs.btrfs`.

2. **`pacman-modified-files.txt` was always empty**
   - **Location:** `omarchy-backup.sh` (`pacman -Qii | awk '/^MODIFIED/'`)
   - **Impact:** Modern `pacman -Qii` outputs indented lines ending with `[modified]` rather than `^MODIFIED`, so `pacman-modified-files.txt` was always 0 bytes.
   - **Fix:** Use `LC_ALL=C pacman -Qii 2>/dev/null | awk '/^Name/{pkg=$3} /\[modified\]/{print pkg "\t" $(NF-1)}'`.

3. **LUKS header backup failed on first run and deleted the previous header before writing a new one**
   - **Location:** `omarchy-backup.sh` (LUKS header step)
   - **Impact:** `$DEST/system/` did not exist yet on a fresh backup (only `$DEST/packages` was created beforehand), causing `cryptsetup luksHeaderBackup` to fail with `No such file or directory`. Furthermore, `rm -f "$hdr"` deleted the existing header backup before the new backup succeeded.
   - **Fix:** Create `$DEST/system` upfront; write the backup to a temporary file on the same encrypted USB filesystem (`.luks-header-*.tmp`), `chmod 600`, and atomically `mv -f` it into place on success.

4. **`--dry-run` still wrote directories to the drive, rotated logs, and ran `--eject`**
   - **Location:** `omarchy-backup.sh` (`mkdir -p`, log rotation, `cleanup` eject condition)
   - **Impact:** `mkdir -p "$DEST/packages"` and `mkdir -p "$DEST/$dest"` mutated the USB drive during `--dry-run`; `--dry-run --eject` unmounted, locked, and powered off the drive; `Dumping package lists` was logged even though skipped; and dry runs evicted real backup logs.
   - **Fix:** Guard directory creation, package-list logging, log rotation, and `--eject` with `(( ! DRY_RUN ))`.

---

### High Severity

5. **`cleanup()` could abort midway under `set -euo pipefail` and power off an unlocked disk**
   - **Location:** `omarchy-backup.sh` (`cleanup()`)
   - **Impact:** `set -e` and `set -o pipefail` remain active inside an `EXIT` trap. If `/sys/class/block/.../slaves` was missing, `ls ... | head -n1` aborted `cleanup()` immediately without logging a warning. Also, if `udisksctl lock` failed, `udisksctl power-off` still ran on a disk with an active dm-crypt mapping.
   - **Fix:** Disable `set -e` / `pipefail` inside `cleanup()`, replace `ls | head` with bash globbing, and only call `udisksctl power-off` when `udisksctl lock` succeeds.

6. **`MIN_FREE_GIB=5` was unused dead code**
   - **Location:** `omarchy-backup.sh`
   - **Impact:** Free space was never checked before running the backup.
   - **Fix:** Check available space on `$MOUNT_POINT` against `MIN_FREE_GIB` (abort on real runs, warn on dry runs).

7. **Safety check in `format-usb.sh` broke on btrfs subvolumes and device symlinks**
   - **Location:** `format-usb.sh` (root/boot/home disk protection)
   - **Impact:** `findmnt -no SOURCE` returns `/dev/mapper/root[/@]` for btrfs subvolumes, causing `lsblk -s` to fail silently and skip the check. Passing a symlink like `/dev/disk/by-id/...` broke `basename "$DISK"` matching.
   - **Fix:** Resolve `DISK="$(readlink -f "$DISK")"`, verify `TYPE == disk`, and use `findmnt -no SOURCE --nofsroot` + `lsblk -no NAME,PKNAME -s`.

8. **Re-formatting an already-unlocked LUKS USB drive failed in `format-usb.sh` and lacked error cleanup**
   - **Location:** `format-usb.sh`
   - **Impact:** Open dm-crypt mappings on the target disk were unmounted but never closed before `wipefs`/`parted`, causing `device or resource busy`. Failures halfway through left `$MNT` mounted and `$MAPPER` open.
   - **Fix:** Close any `crypt` children on `$DISK` before `wipefs`, and add an `EXIT` trap to unmount `$MNT`, remove `$MNT`, and close `$MAPPER` on failure.

9. **TOML config validator loopholes**
   - **Location:** `omarchy-backup.sh` (embedded Python TOML parser)
   - **Impact:** `bool(s.get(k, False))` treated non-boolean strings like `"false"` as `True`; `LAST_BACKUP` and exact `system` were not reserved (allowing `LAST_BACKUP` collision or rsync `--delete` wiping `system/luks-header-*.img`); and non-dict items in `source` caused an unhandled error.
   - **Fix:** Validate `isinstance(s, dict)`, enforce `isinstance(v, bool)` for boolean keys, reserve `LAST_BACKUP` and exact `system`, and reject `\r` in paths/excludes.

10. **Broken or incomplete instructions in `RESTORE.md`**
    - **Location:** `RESTORE.md`
    - **Impact:**
      - `udisksctl unmount -b /dev/mapper/OMARCHY-BACKUP` fails because udisks2 maps LUKS devices as `/dev/mapper/luks-<UUID>`.
      - `pacman -S --needed - < pacman-native.txt` and `yay -S --needed - < pacman-aur.txt` redirect stdin, breaking interactive prompts (`[Y/n]`, PKGBUILD diffs, `sudo`).
      - `flatpak install` inside `while read` lacked `</dev/null`.
      - `/var/lib/iwd/` (Omarchy Wi-Fi profiles), `/var/lib/NetworkManager/`, and `/var/lib/AccountsService/` were missing from the restore steps.
      - Header label text contradicted the mount path and `README.md`.
      - Non-root `rsync -aAXH --numeric-ids` for home restore can fail on xattrs/ownership or UID mismatch.
    - **Fix:** Update all commands and labels in `RESTORE.md`.

---

### Medium / Minor Glitches

11. **Log ownership, lock order, and `tee` flush race**
    - **Location:** `omarchy-backup.sh`
    - **Impact:** Early exits left `$LOG` owned by `root:root`; `mkdir -p "$LOG_DIR"` could create `~/.local/state` owned by root; `flock` ran after opening `$LOG` (creating extra logs on lock contention); and `exec > >(tee ...)` could exit before flushing final log lines to the terminal.
    - **Fix:** Acquire `flock` before logging (and close fd 9 inside the `tee` subshell), create `$LOG_DIR` and `$LOG` as `$USER_NAME`, install `cleanup()` immediately after logging starts, and flush `tee` via `exec >&- 2>&-; wait "$TEE_PID"` before exiting.

12. **Active backup log file was backed up inside `~`**
    - **Location:** `omarchy-backup.toml`
    - **Impact:** `~/.local/state/omarchy-backup/` was copied mid-write and rotated logs were moved to `_deleted/` every run.
    - **Fix:** Exclude `/.local/state/omarchy-backup/` in `omarchy-backup.toml`.

13. **`findmnt` subvolume/escaping handling**
    - **Location:** `omarchy-backup.sh`
    - **Impact:** `findmnt -rn` escaped spaces as `\x20` and included `[subvol]` on btrfs sources, and `findmnt -no FSTYPE "$MOUNT_POINT"` wasn't restricted with `-M -f`.
    - **Fix:** Use `findmnt -lnf --nofsroot` and `findmnt -M "$MOUNT_POINT" -no FSTYPE -f`.

14. **`vfat` rsync branch omitted `--delete-excluded` and used `--modify-window=1`**
    - **Location:** `omarchy-backup.sh`
    - **Impact:** Excluded files added later to a `vfat` source were not pruned, and FAT's 2-second mtime granularity can cause edge-case recopies with `--modify-window=1`.
    - **Fix:** Add `--delete-excluded` and use `--modify-window=2`.

15. **Miscellaneous edge cases**
    - Removed unused `--preserve-env=DISPLAY` from `sudo` re-exec in `omarchy-backup.sh`.
    - Changed `readlink -f "$2"` to `readlink -m "$2"` for `--config` (and added `--config=FILE` support) so missing parent directories produce a clean error.
    - Added `-d '\n'` to `xargs -r` during log pruning for paths containing whitespace.
    - Replaced `sleep 2` in `format-usb.sh` with `udevadm settle`, validated `[[ -b $PART ]]`, and updated the final message to remind the user to replug/unlock the drive before running `--dry-run`.
    - Distinguished "does not exist" vs "is not a directory" for configured sources in `omarchy-backup.sh`.

---

## 2. Improvements Implemented

- **Single-filesystem guard (`-x` / `--one-file-system`):** Enabled by default via a per-source `one_file_system` TOML option (default `true`) to prevent rsync from descending into FUSE/SSHFS/rclone/external mounts under `~`.
- **Dynamic root LUKS detection:** Resolve the root dm-crypt mapper dynamically from `findmnt -no SOURCE --nofsroot /` (falling back to `root`).
- **Read-only mount check:** Abort cleanly on real runs if `$MOUNT_POINT` is mounted read-only.
- **Vanished-file logging:** Log an informational message when rsync exits with code 24.
- **Critical urgency on failure notifications:** Pass `-u critical` to `notify-send` when a backup fails.
- **Portable hostname:** Use `uname -n` (from `coreutils`) once and reuse `$HOST_NAME` in `LAST_BACKUP`.
- **Automated smoke tests (`test.sh`):** Added syntax checks, TOML validator test suite, `pacman -Qii` parser check, and `cleanup()` trap resilience check.
