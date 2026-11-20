# Omarchy Backup Plan

Goal: one script that syncs everything needed to rebuild this Omarchy (Arch + Hyprland) install to an external USB drive. It backs up the home folder, selected system config, and package lists (not the packages themselves).

## Decisions

| Topic | Decision | Status |
|---|---|---|
| Scope | Back up everything, including secrets (`~/.ssh`, `~/.gnupg`, wifi profiles) | Confirmed |
| Excludes | All cache and temp files | Confirmed |
| Method | Plain `rsync` to the USB drive, no backup tool or repo format | Confirmed |
| USB filesystem | ext4 or btrfs, so permissions, ownership, ACLs and xattrs are kept | Assumed, please confirm |
| USB encryption | LUKS strongly recommended, because the backup holds secrets in the clear | Assumed, please confirm |
| Deletions | Mirror with `--delete`, but moved into a dated `_deleted/` dir (kept for 30 days) | Assumed, please confirm |

## Current system facts (checked on this machine)

- Root is btrfs on LUKS (`/dev/sda2`). `/boot` is a separate vfat partition with Limine.
- Home is about 43G, of which `~/.cache` is 24G and `~/.config` is 4.6G.
- Tools present: `rsync`, `flatpak`, `yay`.
- Package counts: about 239 explicit native, 6 foreign/AUR, 7 flatpak apps.
- No USB drive was attached when this plan was written, so the device name and label are still unknown.
- A Windows 11 install sits on the NVMe drive. It is out of scope.

## What gets backed up

### 1. Home folder: `/home/martin/` → `<USB>/omarchy-backup/<hostname>/home/`

`rsync -aAXH --numeric-ids --delete`

Excludes (cache and temp):

```
.cache/
.local/share/Trash/
.local/state/**/cache*          # only if present
.thumbnails/
.npm/_cacache/
.cargo/registry/cache/
.cargo/registry/src/
.rustup/tmp/
.var/app/*/cache/               # flatpak app caches
.config/*/Cache/ .config/*/Code Cache/ .config/*/GPUCache/
.config/*/CachedData/ .config/*/Service Worker/CacheStorage/
.mozilla/firefox/*/cache2/
*.tmp  *.temp  *~  .*.swp
lost+found/
**/node_modules/               # regenerable; see Open questions
**/__pycache__/
```

Browser profiles, SSH, GPG, dotfiles and `~/Development` are kept.

### 2. System config: `/etc/` → `<USB>/omarchy-backup/<hostname>/system/etc/`

- Needs root (`sudo rsync -aAXH --numeric-ids --delete /etc/ ...`).
- Includes: `fstab`, `crypttab`, `mkinitcpio.conf*`, `pacman.conf`, `pacman.d/` (mirrorlist, hooks), `NetworkManager/system-connections/` (wifi passwords), `snapper/`, `limine*`, `sudoers.d/`, `systemd/`, `ssh/`, `hosts`, `locale.*`, `X11/`, `environment`.
- Excludes: `/etc/ssl/certs` (regenerated), `*.pacnew` and `*.pacsave` are kept because they show local changes.

### 3. Other system pieces

Copied to `<USB>/omarchy-backup/<hostname>/system/`:

- `/boot/limine.conf` and `/boot/EFI/` (the bootloader config). `/boot` is vfat, so this uses `rsync -rt` without ownership.
- `/usr/local/` (local scripts and binaries).
- `/var/lib/iwd/` if iwd is in use, and `/var/lib/AccountsService/` if present.
- `/opt/` listing only, not the contents.
- Selected `/var/lib/` state: `bluetooth/` (paired devices) and `NetworkManager/`.
- The LUKS header: `cryptsetup luksHeaderBackup /dev/sda2 --header-backup-file luks-header-sda2.img`. This is run once per backup and is very useful if the header is damaged. It must only be stored on the encrypted USB.

### 4. Package lists (lists only, no packages)

Written to `<USB>/omarchy-backup/<hostname>/packages/` and also copied into the home backup so they are versioned with the date:

| File | Command |
|---|---|
| `pacman-native.txt` | `pacman -Qqen` (explicit, official repos) |
| `pacman-aur.txt` | `pacman -Qqem` (foreign, AUR/manual) |
| `pacman-all-explicit.txt` | `pacman -Qqe` |
| `pacman-deps.txt` | `pacman -Qqd` (for reference only) |
| `pacman-versions.txt` | `pacman -Q` |
| `flatpak-apps.txt` | `flatpak list --app --columns=application,origin` |
| `flatpak-remotes.txt` | `flatpak remotes --columns=name,url` |
| `omarchy-version.txt` | `omarchy version` |
| `pacman-pkg-owned-modified-files.txt` | `pacman -Qii \| awk '/^MODIFIED/'` (shows changed config files) |
| `systemd-enabled.txt` | `systemctl list-unit-files --state=enabled` and the same with `--user` |

## Script design: `omarchy-backup.sh`

Location: `~/Development/omarchy-backup/omarchy-backup.sh` (optionally symlinked to `~/.local/bin/`).

Flow:

1. **Safety checks**
   - `set -euo pipefail`. Refuse to run as a regular user for the `/etc` step. Either re-run with `sudo` or use `sudo` inside the script.
   - Take a lock (`flock`) so two runs cannot overlap.
2. **Find the USB drive**
   - Use a fixed label or UUID from a config variable (`BACKUP_LABEL`) or the first argument.
   - Mount it if needed (`udisksctl mount`, or `cryptsetup open` plus `mount` for LUKS).
   - Check the mountpoint is really a mount, and that it holds a marker file `.omarchy-backup-target`. This prevents syncing into the wrong place or onto the root disk.
   - Abort if the drive has less free space than the estimated size.
3. **Dump package lists** (section 4 above).
4. **Sync system pieces** (sections 2 and 3).
5. **Sync home** (section 1).
6. **Finish**
   - Write `LAST_BACKUP` with timestamp, hostname, kernel and the script's exit status.
   - Run `sync`, then unmount and optionally power off the drive (`--eject`).
   - Send a desktop notification (`notify-send`) on success or failure.
7. **Options**
   - `-n` / `--dry-run`: passes `--dry-run --itemize-changes` to every rsync.
   - `--eject`: unmount after the backup.
   - `-v`: verbose output.
   - Log to `~/.local/state/omarchy-backup/backup-YYYYmmdd-HHMMSS.log` and keep the last 10 logs.

Each rsync call uses `--delete --delete-excluded --backup --backup-dir=<USB>/omarchy-backup/<hostname>/_deleted/<date>` (when the deletion decision stays as assumed). Old `_deleted/` dirs are pruned after 30 days.

Exit codes: rsync `0` and `24` (files vanished while reading) are treated as success. Any other code fails the run.

## Restore outline (to be written as `RESTORE.md` once the script exists)

1. Install Omarchy fresh on the new disk (with matching LUKS and btrfs layout).
2. Mount the USB drive.
3. Reinstall packages:
   - `pacman -S --needed - < pacman-native.txt`
   - `yay -S --needed - < pacman-aur.txt`
   - `xargs -a flatpak-apps.txt flatpak install -y`
4. Restore `/etc` selectively with `rsync` as root, and diff against the new one first (`fstab`, `crypttab` and the bootloader config depend on the new disk's UUIDs, so do not blindly overwrite them).
5. Restore home: `rsync -aAXH <USB>/omarchy-backup/<hostname>/home/ /home/martin/`.
6. Reboot, run `omarchy update`, and check that services (`systemd-enabled.txt`) are enabled.

## Open questions and risks

- **Secrets on the USB.** The backup includes SSH keys, GPG keys, wifi passwords and browser data in plain form. An unencrypted USB is a real exposure if the drive is lost. LUKS on the USB is recommended.
- **Windows dual boot.** Not covered. The Windows disk is not backed up.
- **`node_modules`/build dirs.** Excluded as regenerable. Say if you want them kept.
- **Live files.** Browser databases and similar files change while syncing. For a consistent copy, close the browser before running, or take a btrfs snapshot first and sync from the snapshot. A snapshot-based sync is a recommended later upgrade.
- **Not a full image.** This backup rebuilds the setup but is not a bootable system clone. The bootloader is reinstalled by the fresh Omarchy install.
- **Automation.** The script is manual to start. A systemd user timer or udev trigger on plugging in the drive could be added later.

## Next steps

1. Confirm the assumptions in the Decisions table (USB filesystem, encryption, deletion behaviour).
2. Attach the USB drive, so the script can be configured with its label or UUID.
3. Implement `omarchy-backup.sh` and test it with `--dry-run`.
4. Do a real run, then test a partial restore of a few files.
5. Write `RESTORE.md`.
