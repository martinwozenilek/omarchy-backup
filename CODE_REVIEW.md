# Code review: omarchy-backup

Reviewed on 2026-11-26 at commit `8f53fe7` (main). Scope: every file under `src/` and `tests/`, plus `omarchy-backup.toml`, `README.md` and `RESTORE.md`.

## Verdict

The code is in good shape. `cargo test` (30 tests) and `cargo clippy --all-targets -- -D warnings` both pass on rustc 1.99.1. The safety-critical paths (marker check, btrfs check, read-only check, free-space check, reserved and overlapping `dest` validation, USB-only and protected-disk refusal in `format-usb`) are correct and tested. No data-loss bug was found.

The real output of the last run on the mounted drive was checked as evidence: package lists, flatpak lists, `omarchy-version.txt`, the LUKS header (`0600`, no `.tmp` leftover) and `LAST_BACKUP` are all present and correct. A `--dry-run` against a destination that does not exist yet was verified to work with the installed rsync 3.5.1.

The findings below are ordered by impact. None is a blocker.

## Medium: behaviour that does not match intent

### 1. An interrupted run is reported as a failed run

`src/backup.rs:275` returns exit 130 without logging anything, and `execute_backup` sends the critical "Backup FAILED" notification for every non-zero code, including 130. Ctrl-C during the package dump is worse: `pacman` receives the terminal SIGINT too, `capture_required` in `src/packages.rs` sees a failed command, and the run ends with exit 1 instead of 130 (from reading the code, not reproduced).

Interrupting during rsync works correctly. A harness calling `run_rsync` returned `Interrupted` in 40 of 40 process-group SIGINT runs and 10 of 10 single-process SIGTERM runs, with no temp file left in the destination.

Fix: check the interrupt flag before treating any step's error as a failure, log "Backup interrupted", and use a distinct notification for exit 130.

### 2. A failing best-effort command overwrites a good package list

`capture_best_effort` in `src/packages.rs` writes whatever the command printed, or nothing if it could not start, regardless of exit status. If `flatpak`, `omarchy` or `systemctl --user` fails during one run, the list from the previous good run is replaced by an empty or partial file. Nothing is logged, and `packages/` has no `_deleted/` copy. The `pacman -Qii` block has the same pattern.

Fix: write only on success, keep the old file otherwise, and log a warning. `pacman -Qqem` exits 1 when there are no foreign packages, so treat that case as an empty list.

### 3. `compress=zstd:3` from `format-usb` never applies to real backups

`src/format_usb.rs:226` mounts with `noatime,compress=zstd:3` only for the few milliseconds needed to create the marker file. Every real backup goes through the udisks automount, which uses its own options. Verified on this machine: the drive is currently mounted `rw,nosuid,nodev,relatime,space_cache=v2` with no compression, so the whole backup is uncompressed.

If compression is wanted, make it a filesystem property instead of a mount option: run `btrfs property set <mnt> compression zstd` on the top-level directory right after the mount in `format-usb`. New files and directories inherit it regardless of mount options. Existing directories do not inherit it retroactively, so on the current drive the property has to be set on every existing directory (for example with `find <mnt> -type d -exec btrfs property set {} compression zstd \;`). `btrfs filesystem defragment -r -czstd` additionally compresses data already written. If compression is not wanted, drop the option so the code does not promise something it does not deliver.

### 4. `_deleted/` copies of home files are world-readable while the drive is mounted

Verified on the drive: `home/` is `drwx------` and owned by the user, but `_deleted/<stamp>/` and `_deleted/<stamp>/home/` are `drwxr-xr-x root`, because rsync creates the backup-dir prefix with the root umask. Files keep their own modes, so `~/.ssh` keys stay `0600`, but any `0644` document that was overwritten or deleted becomes readable by other local users while the drive is unlocked. On a single-user desktop this is low risk, but it is inconsistent with the live copy.

Fix: in `src/backup.rs` create `dest.join("_deleted")` with `create_dir_all` and `set_permissions(0o700)` before the rsync loop (one line, applies to all stamps beneath it).

### 5. Log rotation only runs on successful backups

`src/backup.rs:255` calls `rotate_logs` inside the success path. A run that fails (missing drive, rsync error, interrupt) never rotates, so repeated failures accumulate logs without bound. Move the call into `execute_backup` so it runs on every real (non-dry) run after the log file is created. The current log is always the newest so it is never deleted.

## Low: robustness and hardening

### 6. `format-usb` does not wipe signatures inside the old partitions

`src/format_usb.rs:171` runs `wipefs -a` on the whole disk only. That removes the GPT, not the LUKS header sitting at the start of the old first partition. The new partition is created at the same 1 MiB offset, so between `partprobe` and `luksFormat` the kernel and udisks see the old LUKS signature again (the desktop may pop up an unlock prompt), and `cryptsetup luksFormat` asks an extra "existing LUKS signature, are you sure" question. Run `wipefs -a` on each child partition (the list already collected in `unmount_and_close_existing`) before wiping the disk.

### 7. Protected-disk check covers only `/`, `/boot` and `/home`

`src/format_usb.rs:132`. The `tran=usb` requirement already excludes internal disks, but a USB disk carrying `/var`, `/efi`, `/srv`, swap or a bind mount is not protected. A more general rule: collect `lsblk -o MOUNTPOINTS` for the disk's descendants and refuse if any mount point is outside `/run/media/`, `/media/` or `/mnt/`. This also removes the need for the hard-coded list.

### 8. LUKS header file name depends on kernel device enumeration

`src/luks.rs:68` names the file `luks-header-<devname>.img`, currently `luks-header-sda2.img`. If the USB drive is ever plugged in before boot and enumerates as `sda`, the root disk becomes `sdb2` and a second header file appears beside the first. Name the file by the stable LUKS UUID instead (`cryptsetup luksUUID <dev>`), and mention the device name inside a small sidecar text file or in `LAST_BACKUP`. `RESTORE.md` would need the matching update.

### 9. No transfer summary in non-verbose logs

Without `-v`, rsync prints nothing on success, so a normal run's log contains only the `rsync: <source>` lines and the final free-space line (verified in `backup-20261126-200030.log`). Adding `--stats` to `build_rsync_args` in `src/sync.rs` gives a per-source summary (files transferred, bytes, speedup) at the cost of a dozen lines each. `--human-readable` at `src/sync.rs:39` and `:49` currently has no visible effect without it.

### 10. vfat source directories get epoch timestamps

Verified: `system/boot` on the drive has mtime `Jan 1 1970`, because `-rt` copies directory times from the FAT filesystem, whose root directory has none. Add `-O` (`--omit-dir-times`) to the vfat flag set at `src/sync.rs:35`. Cosmetic, but it also stops rsync from re-touching the directory on every run.

### 11. Nested btrfs subvolumes are silently skipped by `-x`

`one_file_system = true` passes `-x`, and rsync treats each btrfs subvolume as a separate filesystem (different `st_dev`). On this machine `/home` is the `@home` subvolume and no nested subvolumes exist under `~`, so nothing is lost today. Tools like snapper, podman with the btrfs driver or a manual `btrfs subvolume create` under home would create silent gaps. Worth a sentence in the config comment and README under `one_file_system`, and possibly a one-time warning in the log when a source contains a directory with a different `st_dev` than the source root.

### 12. `format-usb` success message hard-codes a build path

`src/format_usb.rs:247` prints `./target/release/omarchy-backup --dry-run`. When installed elsewhere the hint is wrong. Use the program name from `std::env::current_exe()` or just `omarchy-backup --dry-run`.

### 13. `FormatGuard` can delete a pre-existing mount point

`src/format_usb.rs:78` removes `/mnt/omarchy-backup` on failure even if the directory existed before the run. Record whether the directory was created by this run and only remove it in that case.

### 14. Passphrase is entered three times during `format-usb`

`luksFormat` asks twice, then `cryptsetup open` at `src/format_usb.rs:215` asks again. Acceptable for a one-time setup, but could be reduced by reading the passphrase once in Rust and feeding it to both commands via `--key-file -` on stdin. Trade-off: the passphrase then passes through process memory and a pipe. Fine to leave as is.

## Code quality

### 15. Duplicated JSON structs for `findmnt` and `lsblk`

`FindmntOutput` is defined in `src/target.rs:22`, `src/luks.rs:11` and `src/format_usb.rs:36`; `LsblkOutput` in `src/target.rs:36` and `src/format_usb.rs:17`. The `mount_source` / `find_root_mount_source` functions in `format_usb.rs` and `luks.rs` are near-identical. A small `sysinfo.rs` (or `probe.rs`) module with `findmnt_source(mount_point)`, `findmnt_by_label(label)` and shared lsblk node types would remove about 80 lines and give one place to test the JSON parsing.

### 16. rsync output chunks can interleave mid-line

`src/sync.rs:160` forwards 8 KiB chunks from stdout and stderr under the logger mutex, so a stderr message can land in the middle of a stdout line. Reading each pipe with `BufRead::read_until(b'\n')` and logging whole lines keeps the log readable without losing anything, since rsync emits line-oriented output.

### 17. `Cargo.toml` lacks `rust-version` and release profile settings

Edition 2024 plus `let` chains require Rust 1.88 or newer. Declaring `rust-version = "1.88"` turns a cryptic parse error into a clear message on older toolchains. `[profile.release] strip = true` and `lto = "thin"` are optional but shrink the binary.

### 18. `--eject` combined with `--dry-run` is silently ignored

`src/backup.rs` skips the eject when `dry_run` is set, by design, but logs nothing. One log line ("dry run: --eject ignored") avoids confusion.

## Tests

### 19. The backup-flow test runs the real `flatpak`

`tests/backup_flow.rs` provides mocks for rsync, pacman, cryptsetup, omarchy, systemctl, udisksctl and notify-send, but not for `flatpak`. Because `command_exists("flatpak")` finds `/usr/bin/flatpak` on this machine, the real binary runs with `HOME` pointing at the temp dir. It is harmless but non-hermetic and machine-dependent. Add a `flatpak` mock script next to the others.

### 20. `touch -d "60 days ago"` failure is ignored

`tests/backup_flow.rs:277` uses `let _ =` on the command that back-dates the old `_deleted` directory. If `touch` is missing or the date syntax fails, the later `assert!(!old_dir.exists())` fails with a misleading message. Use `filetime` or `std::fs::File::set_modified` (stable since Rust 1.75) instead of shelling out, and unwrap the result.

### 21. Missing coverage

- The interrupt path (`RsyncOutcome::Interrupted`, exit 130) is untested. A mock rsync that sleeps, plus a thread that sets the flag, covers it.
- The successful `--eject` sequence is untested because `resolve_backing_devices` reads `/sys` directly. Making the resolver injectable (a function parameter or a trait) lets the test assert the `unmount`, `lock`, `power-off` order and the skip-power-off-if-lock-fails rule.
- `prune_old_deleted` keeps a fresh dir and removes an old one, but nothing tests `keep_deleted_days = 0`.
- `format_usb::unmount_and_close_existing` and `locate_first_partition` parse lsblk trees with no unit tests, unlike their siblings.

## Documentation

### 22. Small inconsistencies

- `README.md` says "the last 10 are kept" for logs, which is true only for successful runs (see finding 5).
- `RESTORE.md` refers to `luks-header-sdX2.img`; if finding 8 is adopted this becomes `luks-header-<uuid>.img`.
- The config comment on `one_file_system` should mention btrfs subvolumes (finding 11).
- `README.md` documents `-v`, `--eject`, `--config` but not `-n` as the short form of `--dry-run`.

## What was checked and found correct

For completeness, these were specifically examined and need no change:

- Lock is taken before the log file is created, uses `O_CLOEXEC` and a non-blocking exclusive flock.
- `discover_target` never mounts or unlocks; the marker check cannot be bypassed by a dry run.
- Reserved `dest` prefixes (`packages`, `_deleted`, `LAST_BACKUP`) and exact `system` prevent any source from syncing over metadata or the LUKS header; `--delete` on `system/etc` cannot touch `system/luks-header-*.img`.
- `--backup-dir` lives outside every source destination, so it cannot recurse into itself.
- Exit code 24 is accepted; all other non-zero codes propagate.
- `_deleted/<stamp>` pruning by directory mtime is sound: rsync creates the stamp directory once per run and nothing touches it afterwards (verified against all 17 existing stamp directories on the drive).
- `std::process::Command::uid` drops supplementary groups before `setuid`, so the user-level `flatpak`, `systemctl --user` and `notify-send` calls do not keep root's groups.
- The LUKS header is written to a `.tmp` file, chmodded to `0600` and renamed atomically; failure paths remove the temp file.
- `posix_normpath`, tilde expansion, overlap detection and `deny_unknown_fields` reject every malformed config shape tried in `tests/config.rs`.
