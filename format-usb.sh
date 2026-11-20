#!/usr/bin/env bash
# format-usb.sh - ONE-TIME setup: wipe a USB disk and create GPT + LUKS2 + btrfs for omarchy-backup.sh.
# DESTROYS ALL DATA on the target disk. Usage: sudo ./format-usb.sh /dev/sdb
set -euo pipefail

DISK="${1:?Usage: $0 /dev/sdX}"
LABEL="OMARCHY-BACKUP"; MAPPER="omarchy-backup"; MNT="/mnt/omarchy-backup"

[[ $EUID -eq 0 ]] || exec sudo "$0" "$@"
[[ -b $DISK ]] || { echo "$DISK is not a block device" >&2; exit 1; }

# safety: refuse the disks holding / or /boot, and non-USB disks
for m in / /boot /home; do
  src="$(findmnt -no SOURCE "$m" 2>/dev/null || true)"
  [[ -n $src ]] && lsblk -no PKNAME -s "$src" 2>/dev/null | grep -qx "$(basename "$DISK")" \
    && { echo "Refusing: $DISK holds $m" >&2; exit 1; }
done
[[ $(lsblk -dno TRAN "$DISK") == usb ]] || { echo "Refusing: $DISK is not a USB disk" >&2; exit 1; }

lsblk -o NAME,SIZE,FSTYPE,LABEL,MOUNTPOINT "$DISK"
read -r -p "ERASE EVERYTHING on $DISK? Type the device path to confirm: " answer
[[ $answer == "$DISK" ]] || { echo "Aborted."; exit 1; }

# unmount anything on it
for p in $(lsblk -lnpo NAME "$DISK" | tail -n +2); do umount "$p" 2>/dev/null || true; done

wipefs -a "$DISK"
parted -s "$DISK" mklabel gpt mkpart "$LABEL" 1MiB 100%
partprobe "$DISK"; sleep 2
PART="$(lsblk -lnpo NAME "$DISK" | sed -n 2p)"

cryptsetup luksFormat --type luks2 --label "$LABEL" "$PART"   # prompts for the passphrase
udevadm settle
# the desktop (udisks) may auto-unlock the fresh LUKS volume and hold the partition open: close it
for h in $(ls "/sys/class/block/$(basename "$PART")/holders" 2>/dev/null); do
  umount "/dev/$h" 2>/dev/null || true
  cryptsetup close "$(cat "/sys/class/block/$h/dm/name")"
done
cryptsetup open "$PART" "$MAPPER"
mkfs.btrfs -L "$LABEL" "/dev/mapper/$MAPPER"
mkdir -p "$MNT"
mount -o noatime,compress=zstd:3 "/dev/mapper/$MAPPER" "$MNT"
touch "$MNT/.omarchy-backup-target"
umount "$MNT"
cryptsetup close "$MAPPER"
echo "Done. $PART is ready. Run ./omarchy-backup.sh --dry-run next."
