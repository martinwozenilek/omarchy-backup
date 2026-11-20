# omarchy-backup

Simple `rsync` backup of an [Omarchy](https://omarchy.org/) (Arch Linux + Hyprland) setup to an encrypted external USB drive (LUKS2 + btrfs). It backs up what you need to rebuild the system, but not the packages themselves.

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

## Setup (once)

Format the USB drive. This **destroys all data** on it and asks for a LUKS passphrase:

```bash
sudo ./format-usb.sh /dev/sdX
```

It refuses non-USB disks and the disks holding `/` or `/boot`. It creates GPT, LUKS2 (label `OMARCHY-BACKUP`), btrfs (label `omarchy-backup`) and a marker file `.omarchy-backup-target`.

If the desktop auto-unlocks the new LUKS volume and the script fails with "device in use", create the btrfs filesystem on the already-open `/dev/mapper/...` device by hand.

## Usage

The script does not unlock or mount the drive. Plug it in and unlock it in the file manager, so the desktop automounts it at `/run/media/<user>/omarchy-backup`. Then run:

```bash
./omarchy-backup.sh --dry-run    # preview, writes nothing
./omarchy-backup.sh              # real backup
./omarchy-backup.sh --eject      # real backup, then unmount, lock and power off the drive
./omarchy-backup.sh -v           # verbose
```

The script runs itself through `sudo`. Close your browser first, so its database files are copied consistently. Logs go to `~/.local/state/omarchy-backup/`, and the last 10 are kept.

The script aborts unless the mounted btrfs drive contains the marker file, so it never syncs into the wrong place.

## Restore

See [RESTORE.md](RESTORE.md). In short: reinstall Omarchy, reinstall packages from the lists, restore `/etc` selectively (UUIDs in `fstab`, `crypttab` and the bootloader config differ per disk), then `rsync` the home folder back.

## Security

The backup contains secrets in plain form: SSH and GPG keys, wifi passwords, browser data and the LUKS header. Keep it only on the encrypted drive, and never commit backup data to a repository (see `.gitignore`).

## Files

| File | Purpose |
|---|---|
| `omarchy-backup.sh` | The backup script |
| `format-usb.sh` | One-time USB drive setup |
| `PLAN.md` | Original design and decisions |
| `RESTORE.md` | Restore guide |
| `CLAUDE.md` | Notes for Claude Code |

## Limitations

- Not a bootable image. The bootloader comes from a fresh Omarchy install.
- A Windows install on another disk is not backed up.
- Written for this machine, so check the paths and labels in the config block at the top of `omarchy-backup.sh` before using it elsewhere.
