# Restore Guide

How to rebuild an Omarchy setup from a backup made by `omarchy-backup.sh`. Replace `omarchydesk` below with the hostname folder you want to restore from.

Backup layout on the USB drive (LUKS2 + btrfs, label `OMARCHY-BACKUP`):

```
omarchy-backup/<hostname>/
├── home/          # full copy of /home/martin (without caches/temp files)
├── system/
│   ├── etc/       # /etc
│   ├── boot/      # /boot (limine.conf, EFI)
│   ├── usr-local/ # /usr/local
│   ├── var-lib/   # bluetooth, NetworkManager, iwd, AccountsService
│   └── luks-header-*.img   # LUKS header of the root disk
├── packages/      # package lists (pacman, AUR, flatpak, services)
├── _deleted/      # files deleted/overwritten between runs (kept 30 days)
└── LAST_BACKUP
```

## 0. Prerequisites

- A fresh Omarchy install on the new disk, with the same username (`martin`) and a working network connection.
- The USB drive plugged in.

## 1. Unlock and mount the backup

Plug in the USB drive and open it in the file manager. Enter the LUKS passphrase when asked, and the desktop automounts it at `/run/media/<user>/omarchy-backup`, the same way as for the backup script.

```bash
B=/run/media/$USER/omarchy-backup/omarchy-backup/omarchydesk
cat $B/LAST_BACKUP
```

Without a desktop (for example from a TTY or a live system), unlock and mount by hand:

```bash
sudo cryptsetup open /dev/disk/by-label/OMARCHY-BACKUP omarchy-backup
sudo mkdir -p /mnt/omarchy-backup
sudo mount -o ro /dev/mapper/omarchy-backup /mnt/omarchy-backup
B=/mnt/omarchy-backup/omarchy-backup/omarchydesk
```

## 2. Reinstall packages

```bash
cd $B/packages
sudo pacman -S --needed - < pacman-native.txt
yay -S --needed - < pacman-aur.txt
while IFS=$'\t' read -r app origin; do flatpak install -y "$origin" "$app"; done < flatpak-apps.txt
```

- `pacman-native.txt` and `pacman-aur.txt` list explicitly installed packages. Dependencies are pulled in automatically.
- Packages that no longer exist in the repos are reported by pacman. Remove them from the list and retry.
- `flatpak-remotes.txt` lists the remotes to add first if flatpak does not know them (`flatpak remote-add --if-not-exists <name> <url>`).
- `pacman-versions.txt` and `pacman-deps.txt` are reference only.

## 3. Restore system config (`/etc`), selectively

Do not copy `/etc` blindly. Some files contain UUIDs of the old disk and would break boot.

```bash
# See what differs first
sudo diff -rq $B/system/etc /etc | less
```

Safe to restore as a rule (review each before copying):

```bash
sudo rsync -aAXH --numeric-ids $B/system/etc/NetworkManager/system-connections/ /etc/NetworkManager/system-connections/
sudo rsync -aAXH --numeric-ids $B/system/etc/ssh/        /etc/ssh/
sudo rsync -aAXH --numeric-ids $B/system/etc/sudoers.d/  /etc/sudoers.d/
sudo rsync -aAXH --numeric-ids $B/system/etc/pacman.conf /etc/pacman.conf
sudo rsync -aAXH --numeric-ids $B/system/etc/pacman.d/   /etc/pacman.d/
```

Do **not** overwrite without checking the UUIDs against `lsblk -f` on the new system:

- `/etc/fstab`
- `/etc/crypttab`
- `/etc/mkinitcpio.conf*` and `/etc/mkinitcpio.d/` (rebuild with `sudo mkinitcpio -P` if changed)
- Bootloader config: `/boot/limine.conf` (backup in `system/boot/`, and `/etc/default/limine` if present). Merge entries by hand, or let the fresh install generate them.

Other restores:

```bash
sudo rsync -aAXH --numeric-ids $B/system/usr-local/ /usr/local/
sudo rsync -aAXH --numeric-ids $B/system/var-lib/bluetooth/ /var/lib/bluetooth/   # paired devices
sudo systemctl restart NetworkManager bluetooth
```

## 4. Restore the home folder

Log out of the desktop first, and run this from a TTY (Ctrl+Alt+F3) or as another user, so apps do not overwrite files while they are restored.

```bash
rsync -aAXH --numeric-ids $B/home/ /home/martin/
```

Add `--dry-run --itemize-changes` first to preview. To restore a single file or folder, copy just that path (for example `$B/home/.ssh/`). Older versions of overwritten files are in `$B/_deleted/<timestamp>/home/`.

## 5. Re-enable services

Compare what was enabled on the old system and enable anything missing:

```bash
systemctl list-unit-files --state=enabled | diff - $B/packages/systemd-enabled.txt
systemctl --user list-unit-files --state=enabled | diff - $B/packages/systemd-user-enabled.txt
```

## 6. Finish

```bash
# automount: eject the drive in the file manager, or
udisksctl unmount -b /dev/mapper/OMARCHY-BACKUP
udisksctl lock -b /dev/disk/by-label/OMARCHY-BACKUP
# manual mount instead:
#   sudo umount /mnt/omarchy-backup && sudo cryptsetup close omarchy-backup
omarchy update
sudo reboot
```

After the reboot, check:
- Hyprland, the bar and your theme look right (`omarchy theme set <name>` if needed).
- Wifi and Bluetooth connect, SSH and GPG keys work (`ssh -T git@github.com`, `gpg -K`).
- `$B/packages/pacman-modified-files.txt` lists config files that were changed from the package defaults. Check that those changes are present.

## Restoring only the LUKS header (disaster recovery)

If the root disk's LUKS header is damaged and the disk no longer unlocks, boot a live USB and run:

```bash
sudo cryptsetup luksHeaderRestore /dev/sdX2 --header-backup-file $B/system/luks-header-sdX2.img
```

Check that `sdX2` is the right partition first, because this overwrites the header. Keep the header file only on the encrypted backup drive.

## Notes

- The backup excludes caches, trash, `node_modules` and `__pycache__`. Rebuild or redownload those as needed.
- Windows on the NVMe drive is not part of this backup.
- This is not a bootable image. The bootloader comes from the fresh Omarchy install.
