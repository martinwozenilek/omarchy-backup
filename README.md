# omarchy-backup

Simple Rust CLI wrapping `rsync` to back up an [Omarchy](https://omarchy.org/) (Arch Linux + Hyprland) setup to an encrypted external USB drive (LUKS2 + btrfs). It backs up what you need to rebuild the system, but not the packages themselves.

## What is backed up

- **Home folder**, without caches and temp files (`~/.cache`, trash, thumbnails, `node_modules`, `__pycache__`, browser caches and similar).
- **System files:** `/etc`, `/boot`, `/usr/local`, and selected `/var/lib` state (Bluetooth, NetworkManager, iwd, AccountsService).
- **LUKS header** of the root disk.
- **Package lists:** pacman (native and AUR), flatpak apps and remotes, enabled systemd services, Omarchy version.

Layout on the drive:

```
omarchy-backup/<hostname>/
├── home/  system/  packages/
├── _deleted/<timestamp>/   # files deleted or overwritten since the last run, kept 30 days
└── LAST_BACKUP
```

To change settings (labels, retention, free-space threshold) or what is backed up, edit `omarchy-backup.toml`: adjust `[settings]`, add or remove `[[source]]` entries, or edit `excludes`.

## Build

Requires Rust (`cargo`) and standard Arch/Omarchy tools (`rsync`, `cryptsetup`, `udisksctl`, `findmnt`, `lsblk`, `pacman`, `notify-send`).

```bash
cargo build --release
```

This produces `./target/release/omarchy-backup`.

## Setup (once)

Format the USB drive. This **destroys all data** on it and asks for a LUKS passphrase:

```bash
./target/release/omarchy-backup format-usb /dev/sdX
```

It re-executes itself via `sudo`, refuses non-USB disks and the disks holding `/`, `/boot` or `/home`, and reads `luks_label`, `fs_label`, and `marker` from `omarchy-backup.toml` (falling back to defaults if no config file is found). It creates GPT, LUKS2 (label `OMARCHY-BACKUP`), btrfs (label `omarchy-backup`), and the marker file `.omarchy-backup-target`.

If the desktop auto-unlocks the new LUKS volume and formatting fails with "device in use", create the btrfs filesystem on the already-open `/dev/mapper/...` device by hand.

## Usage

The tool does not unlock or mount the drive. Plug it in and unlock it in the file manager, so the desktop automounts it at `/run/media/<user>/omarchy-backup`. Then run:

```bash
./target/release/omarchy-backup check               # validate omarchy-backup.toml (no root needed)
./target/release/omarchy-backup --dry-run           # preview, writes nothing
./target/release/omarchy-backup                     # real backup
./target/release/omarchy-backup --eject             # real backup, then unmount, lock and power off the drive
./target/release/omarchy-backup -v                  # verbose
./target/release/omarchy-backup --config other.toml # use a different config file
```

You can also pass the `backup` subcommand explicitly (`omarchy-backup backup --dry-run`). When `--config` is omitted, config discovery checks `./omarchy-backup.toml`, `<exe-dir>/omarchy-backup.toml`, and `~/.config/omarchy-backup/config.toml` in order.

Backup and `format-usb` re-execute themselves through `sudo`. Close your browser first, so its database files are copied consistently. Logs go to `~/.local/state/omarchy-backup/`, and the last 10 are kept.

The tool aborts unless the mounted btrfs drive contains the marker file, so it never syncs into the wrong place.

## Tests

```bash
cargo test
cargo clippy --all-targets -- -D warnings
```

## Restore

See [RESTORE.md](RESTORE.md). In short: reinstall Omarchy, reinstall packages from the lists, restore `/etc` selectively (UUIDs in `fstab`, `crypttab` and the bootloader config differ per disk), then `rsync` the home folder back.

## Security

The backup contains secrets in plain form: SSH and GPG keys, wifi passwords, browser data and the LUKS header. Keep it only on the encrypted drive, and never commit backup data to a repository (see `.gitignore`).

## Files

| File | Purpose |
|---|---|
| `Cargo.toml` / `src/` | Rust CLI implementation (`backup`, `format-usb`, `check`) |
| `tests/` | Automated CLI, config, parser, and mocked backup-flow tests |
| `omarchy-backup.toml` | Settings (`[settings]`) and backup sources (`[[source]]` + excludes) |
| `RESTORE.md` | Restore guide |
| `CLAUDE.md` | Notes for Claude Code |

## Limitations

- Not a bootable image. The bootloader comes from a fresh Omarchy install.
- A Windows install on another disk is not backed up.
- Configured for this machine, so review `[settings]` and `[[source]]` in `omarchy-backup.toml` before using it elsewhere.
