# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

Bash scripts that back up an Omarchy (Arch Linux + Hyprland) install to a LUKS2 + btrfs USB drive with plain `rsync`. There is no build system or test suite. `README.md` is the user overview and `RESTORE.md` the restore guide.

- `omarchy-backup.sh`: the backup. It re-runs itself via `sudo`.
- `omarchy-backup.toml`: what gets backed up. Each `[[source]]` has a `path`, a `dest` under the host folder, and optional `optional`, `vfat` and `excludes` (rsync patterns). The script parses it with Python's `tomllib` and validates it.
- `format-usb.sh`: one-time setup that wipes a USB disk (GPT, LUKS2 labelled `OMARCHY-BACKUP`, btrfs, marker file). It refuses non-USB disks and the disks holding `/` or `/boot`.

## Commands

```bash
bash -n omarchy-backup.sh                 # syntax check (shellcheck is not installed)
./omarchy-backup.sh --dry-run             # preview, writes nothing to the drive
./omarchy-backup.sh [-v] [--eject]        # real run; --eject unmounts, locks and powers off the drive
sudo ./format-usb.sh /dev/sdX             # DESTROYS the disk; needs an interactive LUKS passphrase
```

Both scripts need root and, for the LUKS passphrase, a terminal. In a Claude Code session ask the user to run them with the `!` prefix. Do not format or write to `/dev/sdb` without explicit confirmation.

## Architecture of omarchy-backup.sh

Strictly sequential flow, with settings in the config block at the top:

1. Root re-exec, logging to `~/.local/state/omarchy-backup/` (last 10 kept), a `flock` lock, and loading the TOML config (`--config FILE` overrides the default next to the script).
2. Find the drive by its btrfs label (`omarchy-backup`) among the mounted filesystems. The script never unlocks or mounts: the desktop automount (udisks, `/run/media/<user>/...`) must have done that. It aborts unless the marker file `.omarchy-backup-target` exists and the filesystem is btrfs.
3. Everything goes under `<USB>/omarchy-backup/<hostname>/` with `home/`, `system/`, `packages/` and `_deleted/`. Overwritten or deleted files go to `_deleted/<timestamp>/<dest>/` and are pruned after 30 days.
4. Steps: package lists, the LUKS header of the root disk, then one rsync per config source in order (`/etc`, `/boot`, `/usr/local`, selected `/var/lib` dirs, home with the cache/temp excludes).
5. The `cleanup` EXIT trap handles `--eject` (successful runs only) with `udisksctl unmount`, `lock` and `power-off`.

## Constraints

- rsync exit codes 0 and 24 (files vanished during the run) count as success.
- `/boot` is vfat (`vfat = true` in the config), so it is synced with `-rt` and no ownership or ACL flags. Everything else uses `-aAXH --numeric-ids`.
- The backup contains secrets in plain form (`~/.ssh`, `~/.gnupg`, wifi profiles, LUKS header). It must stay on the encrypted drive, and the marker check must never be removed.
- Everything except caches and temp files is backed up. Do not add excludes that drop real data without asking.
- Keep the paths in `README.md` and `RESTORE.md` in sync when the backup layout changes.
- Omarchy rule: never edit files under `/usr/share/omarchy/`. Reading them is fine.
