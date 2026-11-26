# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

Rust CLI (`omarchy-backup`) that backs up an Omarchy (Arch Linux + Hyprland) install to a LUKS2 + btrfs USB drive with plain `rsync`. `README.md` is the user overview and `RESTORE.md` the restore guide.

- `src/`: Single binary with subcommands:
  - default / `backup`: runs the backup (re-executes itself via `sudo` using `current_exe()`).
  - `format-usb <DEVICE>`: one-time setup that wipes a USB disk (GPT, LUKS2 labelled `OMARCHY-BACKUP`, btrfs labelled `omarchy-backup`, marker file). Reads labels/marker from `omarchy-backup.toml` when present and refuses non-USB disks and the disks holding `/`, `/boot` or `/home`.
  - `check`: validates `omarchy-backup.toml` without root.
- `omarchy-backup.toml`: optional `[settings]` (labels, marker, `dest_dir`, retention, `min_free_gib`) and `[[source]]` entries (`path`, `dest`, optional `optional`, `vfat`, `one_file_system` default `true`, `excludes`). Parsed and strictly validated with `serde` + `toml` (`#[serde(deny_unknown_fields)]`).
- `tests/`: Automated CLI, config-validation, `pacman -Qii` parser, and mocked end-to-end backup-flow tests.

## Commands

```bash
cargo test                                         # unit + integration tests
cargo clippy --all-targets -- -D warnings          # lint checks
cargo build --release                              # build ./target/release/omarchy-backup
./target/release/omarchy-backup check              # validate config without root
./target/release/omarchy-backup --dry-run          # preview, writes nothing to the drive
./target/release/omarchy-backup [-v] [--eject]     # real run; --eject unmounts, locks and powers off
./target/release/omarchy-backup format-usb /dev/sdX # DESTROYS the disk; needs interactive LUKS passphrase
```

Backup and `format-usb` need root and, for the LUKS passphrase, a terminal. In a Claude Code session ask the user to run them with the `!` prefix. Do not format or write to `/dev/sdb` without explicit confirmation.

## Architecture

Strictly sequential backup flow across `src/`:

1. **Privilege, lock, logging, config** (`privilege.rs`, `lock.rs`, `logging.rs`, `config.rs`): Root re-exec via `sudo`, non-blocking `O_CLOEXEC` `flock` on `/run/omarchy-backup.lock`, dual terminal + user-owned log writer in `~/.local/state/omarchy-backup/` (last `keep_logs` kept on real runs), and TOML config loading (`--config FILE`, `./omarchy-backup.toml`, `<exe-dir>/omarchy-backup.toml`, `~/.config/omarchy-backup/config.toml`).
2. **Target discovery & validation** (`target.rs`): Find the drive by its btrfs label (`fs_label`) via `findmnt --json --nofsroot`. Never unlocks or mounts: the desktop automount (`udisks`, `/run/media/<user>/...`) must have done that. Aborts unless the marker file exists, filesystem is `btrfs`, it is mounted read-write (real runs), and free space via `statvfs` is at least `min_free_gib` (real runs).
3. **Layout**: Everything goes under `<USB>/<dest_dir>/<hostname>/` with `home/`, `system/`, `packages/` and `_deleted/`. Overwritten or deleted files go to `_deleted/<timestamp>/<dest>/` and are pruned after `keep_deleted_days` days.
4. **Steps** (`backup.rs`, `packages.rs`, `luks.rs`, `sync.rs`): Package lists + pure-Rust `pacman -Qii` `[modified]` parser, root LUKS header (detected dynamically from `/` with fallback to `root`, written atomically with mode `0600` on the encrypted USB), then one `rsync` per config source in order with live output streaming.
5. **Cleanup & eject** (`backup.rs`, `target.rs`, `notify.rs`): RAII temp-dir cleanup, `SIGINT`/`SIGTERM` handling, critical desktop notification on failure, `--eject` (successful non-dry runs only) with `sync()`, `udisksctl unmount`, `lock`, and `power-off` (skipping `power-off` if `lock` fails), and deterministic log flushing.

## Constraints

- `rsync` exit codes 0 and 24 (files vanished during the run) count as success.
- `/boot` is vfat (`vfat = true` in the config), so it is synced with `-rt --modify-window=2` and no ownership, ACL, or `--backup-dir` flags. Everything else uses `-aAXH --numeric-ids`.
- The backup contains secrets in plain form (`~/.ssh`, `~/.gnupg`, wifi profiles, LUKS header). It must stay on the encrypted drive, and the marker check must never be removed.
- Everything except caches and temp files is backed up. Do not add excludes that drop real data without asking.
- Keep the paths in `README.md` and `RESTORE.md` in sync when the backup layout changes.
- Omarchy rule: never edit files under `/usr/share/omarchy/`. Reading them is fine.
