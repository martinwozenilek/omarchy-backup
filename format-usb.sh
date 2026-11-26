#!/usr/bin/env bash
# format-usb.sh - ONE-TIME setup: wipe a USB disk and create GPT + LUKS2 + btrfs for omarchy-backup.sh.
# DESTROYS ALL DATA on the target disk. Usage: sudo ./format-usb.sh /dev/sdb
set -euo pipefail

DISK="${1:?Usage: $0 /dev/sdX}"
LUKS_LABEL="OMARCHY-BACKUP"
FS_LABEL="omarchy-backup"
MAPPER="omarchy-backup"
MNT="/mnt/omarchy-backup"

[[ $EUID -eq 0 ]] || exec sudo "$0" "$@"
[[ -b $DISK ]] || { echo "$DISK is not a block device" >&2; exit 1; }
DISK="$(readlink -f "$DISK")"
[[ $(lsblk -dno TYPE "$DISK") == disk ]] || { echo "Refusing: $DISK is not a whole disk (pass e.g. /dev/sdX, not a partition)" >&2; exit 1; }

# safety: refuse the disks holding /, /boot or /home, and non-USB disks
disk_name="$(basename "$DISK")"
for m in / /boot /home; do
  src="$(findmnt -no SOURCE --nofsroot "$m" 2>/dev/null || true)"
  [[ -n $src ]] && lsblk -no NAME,PKNAME -s "$src" 2>/dev/null | grep -qw "$disk_name" \
    && { echo "Refusing: $DISK holds $m" >&2; exit 1; }
done
[[ $(lsblk -dno TRAN "$DISK") == usb ]] || { echo "Refusing: $DISK is not a USB disk" >&2; exit 1; }

lsblk -o NAME,SIZE,FSTYPE,LABEL,MOUNTPOINT "$DISK"
read -r -p "ERASE EVERYTHING on $DISK? Type the device path to confirm: " answer
[[ $answer == "$DISK" ]] || { echo "Aborted."; exit 1; }

cleanup() {
  local rc=$?
  set +e +o pipefail
  trap - EXIT
  mountpoint -q "$MNT" 2>/dev/null && umount "$MNT" 2>/dev/null
  [[ -d $MNT ]] && rmdir "$MNT" 2>/dev/null
  [[ -e /dev/mapper/$MAPPER ]] && cryptsetup close "$MAPPER" 2>/dev/null
  exit "$rc"
}
trap cleanup EXIT

# unmount partitions/mappers (deepest first) and close any open LUKS mappings on this disk
while IFS= read -r dev; do
  [[ -n $dev ]] && umount "$dev" 2>/dev/null || true
done < <(lsblk -lnpo NAME "$DISK" | tac | head -n -1)
while IFS= read -r dm; do
  [[ -n $dm ]] && cryptsetup close "$dm" 2>/dev/null || true
done < <(lsblk -lnpo TYPE,NAME "$DISK" | awk '$1 == "crypt" {print $2}')

wipefs -a "$DISK"
parted -s "$DISK" mklabel gpt mkpart "$LUKS_LABEL" 1MiB 100%
partprobe "$DISK"
udevadm settle
PART="$(lsblk -lnpo NAME "$DISK" | sed -n 2p)"
[[ -b ${PART:-} ]] || { echo "Failed to locate newly created partition on $DISK" >&2; exit 1; }

cryptsetup luksFormat --type luks2 --label "$LUKS_LABEL" "$PART"   # prompts for the passphrase
udevadm settle
# the desktop (udisks) may auto-unlock the fresh LUKS volume and hold the partition open: close it
holders_dir="/sys/class/block/$(basename "$PART")/holders"
if [[ -d $holders_dir ]]; then
  for h in "$holders_dir"/*; do
    [[ -e $h ]] || continue
    hname="$(basename "$h")"
    umount "/dev/$hname" 2>/dev/null || true
    [[ -r $h/dm/name ]] && cryptsetup close "$(cat "$h/dm/name")" 2>/dev/null || true
  done
fi

cryptsetup open "$PART" "$MAPPER"
mkfs.btrfs -L "$FS_LABEL" "/dev/mapper/$MAPPER"
mkdir -p "$MNT"
mount -o noatime,compress=zstd:3 "/dev/mapper/$MAPPER" "$MNT"
touch "$MNT/.omarchy-backup-target"
umount "$MNT"
rmdir "$MNT"
cryptsetup close "$MAPPER"
echo "Done. $PART is ready. Unplug and replug the drive (or unlock it in the file manager), then run ./omarchy-backup.sh --dry-run."
