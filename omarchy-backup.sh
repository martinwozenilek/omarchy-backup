#!/usr/bin/env bash
# omarchy-backup.sh - rsync backup of an Omarchy setup to a LUKS+btrfs USB drive.
# See PLAN.md. Run as: ./omarchy-backup.sh [-n] [-v] [--eject]   (re-executes itself via sudo)
set -euo pipefail

# ---------------------------------------------------------------- config
FS_LABEL="omarchy-backup"           # btrfs label of the backup filesystem (set by format-usb.sh)
MARKER=".omarchy-backup-target"     # must exist at the root of the USB fs
DEST_DIR="omarchy-backup"           # folder on the USB fs
KEEP_DELETED_DAYS=30
KEEP_LOGS=10
MIN_FREE_GIB=5

# ---------------------------------------------------------------- args
DRY_RUN=0; VERBOSE=0; EJECT=0
for a in "$@"; do
  case "$a" in
    -n|--dry-run) DRY_RUN=1 ;;
    -v|--verbose) VERBOSE=1 ;;
    --eject)      EJECT=1 ;;
    -h|--help)    sed -n '2,3p' "$0"; exit 0 ;;
    *) echo "Unknown option: $a" >&2; exit 2 ;;
  esac
done

# ---------------------------------------------------------------- root + user
if [[ $EUID -ne 0 ]]; then
  exec sudo --preserve-env=DISPLAY "$0" "$@"
fi
# the invoking user: set by sudo; if started directly as root fall back to the login user
USER_NAME="${SUDO_USER:-$(logname 2>/dev/null || true)}"
if [[ -z $USER_NAME || $USER_NAME == root ]] || ! getent passwd "$USER_NAME" >/dev/null; then
  echo "Cannot determine the user to back up. Run as a normal user (the script uses sudo itself)." >&2
  exit 1
fi
USER_HOME="$(getent passwd "$USER_NAME" | cut -d: -f6)"
USER_UID="$(id -u "$USER_NAME")"

# ---------------------------------------------------------------- logging
LOG_DIR="$USER_HOME/.local/state/omarchy-backup"
mkdir -p "$LOG_DIR"; chown "$USER_NAME": "$LOG_DIR"
STAMP="$(date +%Y%m%d-%H%M%S)"
LOG="$LOG_DIR/backup-$STAMP.log"
exec > >(tee -a "$LOG") 2>&1
log() { printf '[%s] %s\n' "$(date +%T)" "$*"; }

notify() {  # best effort desktop notification as the real user
  sudo -u "$USER_NAME" DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$USER_UID/bus" \
    notify-send -a omarchy-backup "$1" "$2" 2>/dev/null || true
}

# ---------------------------------------------------------------- lock
exec 9>/run/omarchy-backup.lock
flock -n 9 || { echo "Another backup is already running." >&2; exit 1; }

# ---------------------------------------------------------------- locate the (auto-mounted) drive
# The drive is unlocked and mounted by the desktop (udisks automount, /run/media/<user>/...).
# This script never unlocks or mounts anything itself.
FS_DEV="$(findmnt -rn -S "LABEL=$FS_LABEL" -o SOURCE | head -n1 || true)"
MOUNT_POINT="$(findmnt -rn -S "LABEL=$FS_LABEL" -o TARGET | head -n1 || true)"
if [[ -z $FS_DEV || -z $MOUNT_POINT ]]; then
  log "Backup drive (btrfs label $FS_LABEL) is not mounted. Plug it in and unlock it in the file manager, then retry."
  exit 1
fi
[[ -f $MOUNT_POINT/$MARKER ]] || { log "Marker $MARKER missing on $MOUNT_POINT: wrong drive. Aborting."; exit 1; }
[[ $(findmnt -no FSTYPE "$MOUNT_POINT") == btrfs ]] || { log "Target is not btrfs. Aborting."; exit 1; }
log "Using $FS_DEV mounted at $MOUNT_POINT"

cleanup() {
  local rc=$?
  if (( rc != 0 )); then notify "Backup FAILED" "See $LOG"; fi
  # --eject (successful runs only): unmount, lock the LUKS volume and power off the disk via udisks
  if (( EJECT && rc == 0 )); then
    sync
    local luks_part disk
    # lsblk PKNAME is empty for device-mapper devices, so use sysfs to find the LUKS partition
    luks_part="$(ls "/sys/class/block/$(basename "$(readlink -f "$FS_DEV")")/slaves" 2>/dev/null | head -n1)"
    disk=""
    [[ -n $luks_part ]] && { luks_part="/dev/$luks_part"; disk="$(lsblk -no PKNAME "$luks_part" | head -n1)"; }
    if [[ -z $luks_part || -z $disk ]]; then
      log "WARNING: could not determine the USB device behind $FS_DEV; drive left mounted."
    elif disk="/dev/$disk" && udisksctl unmount -b "$FS_DEV" >/dev/null; then
      udisksctl lock -b "$luks_part" >/dev/null || log "WARNING: could not lock $luks_part"
      udisksctl power-off -b "$disk" >/dev/null \
        && log "Drive ejected and powered off - safe to unplug." \
        || log "Unmounted and locked, but power-off failed."
    else
      log "WARNING: could not unmount $MOUNT_POINT (busy?). Drive left mounted."
    fi
  fi
  chown "$USER_NAME": "$LOG" 2>/dev/null || true
  exit "$rc"
}
trap cleanup EXIT

HOST_NAME="$(hostname)"
DEST="$MOUNT_POINT/$DEST_DIR/$HOST_NAME"
DELETED="$DEST/_deleted/$STAMP"
mkdir -p "$DEST"/{home,packages,system}

# ---------------------------------------------------------------- rsync wrapper
RSYNC_COMMON=(-aAXH --numeric-ids --delete --delete-excluded --human-readable)
(( DRY_RUN )) && RSYNC_COMMON+=(--dry-run --itemize-changes)
(( VERBOSE )) && RSYNC_COMMON+=(-v)

run_rsync() {  # run_rsync <label> <args...>; exit codes 0 and 24 are success
  local label=$1; shift
  log "rsync: $label"
  local rc=0
  rsync "$@" || rc=$?
  if (( rc != 0 && rc != 24 )); then log "rsync failed for $label (exit $rc)"; exit "$rc"; fi
}
backup_dir_for() { (( DRY_RUN )) && return 0; echo "--backup" "--backup-dir=$DELETED/$1"; }

# ---------------------------------------------------------------- excludes
EXCLUDES="$(mktemp)"; trap 'rc=$?; rm -f "$EXCLUDES"; (exit $rc); cleanup' EXIT
cat > "$EXCLUDES" <<'EOF'
/.cache/
/.local/share/Trash/
/.thumbnails/
/.npm/_cacache/
/.cargo/registry/cache/
/.cargo/registry/src/
/.rustup/tmp/
/.var/app/*/cache/
/.config/*/Cache/
/.config/*/Code Cache/
/.config/*/GPUCache/
/.config/*/CachedData/
/.config/*/Service Worker/CacheStorage/
/.mozilla/firefox/*/cache2/
node_modules/
__pycache__/
lost+found/
*.tmp
*.temp
*~
.*.swp
EOF

# ---------------------------------------------------------------- 1. package lists
log "Dumping package lists"
PK="$DEST/packages"
if (( ! DRY_RUN )); then
  pacman -Qqen > "$PK/pacman-native.txt"
  pacman -Qqem > "$PK/pacman-aur.txt" || true
  pacman -Qqe  > "$PK/pacman-all-explicit.txt"
  pacman -Qqd  > "$PK/pacman-deps.txt"
  pacman -Q    > "$PK/pacman-versions.txt"
  pacman -Qii 2>/dev/null | awk '/^MODIFIED/' > "$PK/pacman-modified-files.txt" || true
  if command -v flatpak >/dev/null; then
    sudo -u "$USER_NAME" flatpak list --app --columns=application,origin > "$PK/flatpak-apps.txt" || true
    sudo -u "$USER_NAME" flatpak remotes --columns=name,url > "$PK/flatpak-remotes.txt" || true
  fi
  { omarchy version 2>/dev/null || true; } > "$PK/omarchy-version.txt"
  systemctl list-unit-files --state=enabled > "$PK/systemd-enabled.txt" || true
  sudo -u "$USER_NAME" XDG_RUNTIME_DIR="/run/user/$USER_UID" systemctl --user list-unit-files --state=enabled \
    > "$PK/systemd-user-enabled.txt" 2>/dev/null || true
fi

# ---------------------------------------------------------------- 2. system files
# shellcheck disable=SC2046
run_rsync "/etc" "${RSYNC_COMMON[@]}" $(backup_dir_for etc) /etc/ "$DEST/system/etc/"
mkdir -p "$DEST/system/etc" "$DEST/system/boot" "$DEST/system/usr-local" "$DEST/system/var-lib"

# /boot is vfat: no ownership/permissions
run_rsync "/boot" -rt --modify-window=1 --delete $( (( DRY_RUN )) && echo "--dry-run --itemize-changes" ) \
  /boot/ "$DEST/system/boot/"

# shellcheck disable=SC2046
run_rsync "/usr/local" "${RSYNC_COMMON[@]}" $(backup_dir_for usr-local) /usr/local/ "$DEST/system/usr-local/"

for d in bluetooth NetworkManager iwd AccountsService; do
  if [[ -d /var/lib/$d ]]; then
    mkdir -p "$DEST/system/var-lib/$d"
    # shellcheck disable=SC2046
    run_rsync "/var/lib/$d" "${RSYNC_COMMON[@]}" $(backup_dir_for "var-lib-$d") "/var/lib/$d/" "$DEST/system/var-lib/$d/"
  fi
done

# LUKS header of the root disk (kept only on the encrypted USB)
if (( ! DRY_RUN )); then
  ROOT_LUKS_DEV="$(cryptsetup status root 2>/dev/null | awk '/device:/{print $2}' || true)"
  if [[ -n $ROOT_LUKS_DEV ]]; then
    hdr="$DEST/system/luks-header-$(basename "$ROOT_LUKS_DEV").img"
    rm -f "$hdr"
    cryptsetup luksHeaderBackup "$ROOT_LUKS_DEV" --header-backup-file "$hdr" && chmod 600 "$hdr" \
      || log "WARNING: LUKS header backup failed"
  else
    log "WARNING: could not detect root LUKS device, skipping header backup"
  fi
fi

# ---------------------------------------------------------------- 3. home
# shellcheck disable=SC2046
run_rsync "home ($USER_HOME)" "${RSYNC_COMMON[@]}" --exclude-from="$EXCLUDES" $(backup_dir_for home) \
  "$USER_HOME/" "$DEST/home/"

# ---------------------------------------------------------------- 4. finish
if (( ! DRY_RUN )); then
  # prune old deleted/overwritten files
  find "$DEST/_deleted" -mindepth 1 -maxdepth 1 -type d -mtime +"$KEEP_DELETED_DAYS" -exec rm -rf {} + 2>/dev/null || true
  {
    echo "date: $(date -Is)"
    echo "host: $(hostname)"
    echo "kernel: $(uname -r)"
  } > "$DEST/LAST_BACKUP"
  sync
fi
ls -1t "$LOG_DIR"/backup-*.log 2>/dev/null | tail -n +$((KEEP_LOGS + 1)) | xargs -r rm -f

log "Backup finished$( (( DRY_RUN )) && echo ' (dry run)' ). Free space: $(df -h --output=avail "$MOUNT_POINT" | tail -1 | xargs)"
notify "Backup finished" "$( (( DRY_RUN )) && echo 'Dry run' || echo 'OK' )"
