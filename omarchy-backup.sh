#!/usr/bin/env bash
# omarchy-backup.sh - rsync backup of an Omarchy setup to a LUKS+btrfs USB drive.
# See README.md. Run as: ./omarchy-backup.sh [-n] [-v] [--eject] [--config FILE]   (re-executes itself via sudo)
set -euo pipefail

# ---------------------------------------------------------------- config
FS_LABEL="omarchy-backup"           # btrfs label of the backup filesystem (set by format-usb.sh)
MARKER=".omarchy-backup-target"     # must exist at the root of the USB fs
DEST_DIR="omarchy-backup"           # folder on the USB fs
KEEP_DELETED_DAYS=30
KEEP_LOGS=10
MIN_FREE_GIB=5
# what to back up (sources, destinations, excludes); override with --config FILE
CONFIG="$(dirname "$(readlink -f "$0")")/omarchy-backup.toml"

# ---------------------------------------------------------------- args
DRY_RUN=0; VERBOSE=0; EJECT=0
ARGS=("$@")
while (( $# )); do
  case "$1" in
    -n|--dry-run) DRY_RUN=1 ;;
    -v|--verbose) VERBOSE=1 ;;
    --eject)      EJECT=1 ;;
    --config)     [[ -n ${2:-} ]] || { echo "--config needs a file" >&2; exit 2; }
                  CONFIG="$(readlink -m "$2")"; shift ;;
    --config=*)   [[ -n ${1#*=} ]] || { echo "--config needs a file" >&2; exit 2; }
                  CONFIG="$(readlink -m "${1#*=}")" ;;
    -h|--help)    sed -n '2,3p' "$0"; exit 0 ;;
    *) echo "Unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done
set -- "${ARGS[@]}"

# ---------------------------------------------------------------- root + user
if [[ $EUID -ne 0 ]]; then
  exec sudo "$0" "$@"
fi
# the invoking user: set by sudo; if started directly as root fall back to the login user
USER_NAME="${SUDO_USER:-$(logname 2>/dev/null || true)}"
if [[ -z $USER_NAME || $USER_NAME == root ]] || ! getent passwd "$USER_NAME" >/dev/null; then
  echo "Cannot determine the user to back up. Run as a normal user (the script uses sudo itself)." >&2
  exit 1
fi
USER_HOME="$(getent passwd "$USER_NAME" | cut -d: -f6)"
USER_UID="$(id -u "$USER_NAME")"

# ---------------------------------------------------------------- lock
exec 9>/run/omarchy-backup.lock
flock -n 9 || { echo "Another backup is already running." >&2; exit 1; }

# ---------------------------------------------------------------- logging
LOG_DIR="$USER_HOME/.local/state/omarchy-backup"
sudo -u "$USER_NAME" mkdir -p "$LOG_DIR"
STAMP="$(date +%Y%m%d-%H%M%S)"
LOG="$LOG_DIR/backup-$STAMP.log"
sudo -u "$USER_NAME" touch "$LOG"
exec > >(exec 9>&-; tee -a "$LOG") 2>&1
TEE_PID=$!
log() { printf '[%s] %s\n' "$(date +%T)" "$*"; }

notify() {  # best effort desktop notification as the real user: notify <title> <body> [urgency]
  local urgency="${3:-normal}"
  sudo -u "$USER_NAME" DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$USER_UID/bus" \
    notify-send -a omarchy-backup -u "$urgency" "$1" "$2" 2>/dev/null || true
}

WORK=""
FS_DEV=""
MOUNT_POINT=""

cleanup() {
  local rc=$?
  set +e +o pipefail
  trap - EXIT
  [[ -n ${WORK:-} ]] && rm -rf "$WORK"
  if (( rc != 0 )); then
    notify "Backup FAILED" "See $LOG" critical
  fi
  # --eject (successful non-dry runs only): unmount, lock the LUKS volume and power off the disk via udisks
  if (( EJECT && ! DRY_RUN && rc == 0 )) && [[ -n ${FS_DEV:-} ]]; then
    sync
    local dev_name luks_part="" disk="" slaves_dir s
    # lsblk PKNAME is empty for device-mapper devices, so use sysfs to find the LUKS partition
    dev_name="$(basename "$(readlink -f "$FS_DEV" 2>/dev/null)" 2>/dev/null)"
    slaves_dir="/sys/class/block/$dev_name/slaves"
    if [[ -n $dev_name && -d $slaves_dir ]]; then
      for s in "$slaves_dir"/*; do
        [[ -e $s ]] && { luks_part="/dev/$(basename "$s")"; break; }
      done
    fi
    [[ -n $luks_part ]] && disk="$(lsblk -no PKNAME "$luks_part" 2>/dev/null | head -n1)"
    if [[ -z $luks_part || -z $disk ]]; then
      log "WARNING: could not determine the USB device behind $FS_DEV; drive left mounted."
    elif udisksctl unmount -b "$FS_DEV" >/dev/null; then
      if udisksctl lock -b "$luks_part" >/dev/null; then
        udisksctl power-off -b "/dev/$disk" >/dev/null \
          && log "Drive ejected and powered off - safe to unplug." \
          || log "Unmounted and locked, but power-off failed."
      else
        log "WARNING: unmounted, but could not lock $luks_part; skipping power-off."
      fi
    else
      log "WARNING: could not unmount $MOUNT_POINT (busy?). Drive left mounted."
    fi
  fi
  # Flush tee before exiting so final log lines aren't interleaved with the shell prompt
  if [[ -n ${TEE_PID:-} ]]; then
    exec >&- 2>&-
    wait "$TEE_PID" 2>/dev/null || true
  fi
  exit "$rc"
}
trap cleanup EXIT

# ---------------------------------------------------------------- load config
# Parsed with python's tomllib. Prints one line per source:
#   <path> TAB <dest> TAB <optional 0|1> TAB <vfat 0|1> TAB <one_file_system 0|1> TAB <exclude file or empty>
WORK="$(mktemp -d)"
SOURCES="$(python3 - "$CONFIG" "$USER_HOME" "$WORK" <<'PYEOF'
import os, posixpath, sys, tomllib
cfg_path, home, work = sys.argv[1:]
def die(msg): sys.exit(f"Config {cfg_path}: {msg}")
try:
    with open(cfg_path, "rb") as f:
        cfg = tomllib.load(f)
except (OSError, tomllib.TOMLDecodeError) as e:
    die(e)
sources = cfg.get("source")
if not isinstance(sources, list) or not sources:
    die("no [[source]] entries")
known = {"path", "dest", "optional", "vfat", "one_file_system", "excludes"}
seen = []
for i, s in enumerate(sources, 1):
    if not isinstance(s, dict):
        die(f"source {i}: must be a table ([[source]])")
    if unknown := set(s) - known:
        die(f"source {i}: unknown keys {sorted(unknown)}")
    path, dest = s.get("path"), s.get("dest")
    if not isinstance(path, str) or not isinstance(dest, str):
        die(f"source {i}: 'path' and 'dest' are required strings")
    if path == "~" or path.startswith("~/"):
        path = home + path[1:]
    if not path.startswith("/"):
        die(f"source {i}: path must be absolute or start with ~")
    path, dest = posixpath.normpath(path), posixpath.normpath(dest)
    if dest.startswith("/") or dest == "." or ".." in dest.split("/"):
        die(f"source {i}: dest must be a relative path inside the backup")
    if dest.split("/")[0] in ("packages", "_deleted", "LAST_BACKUP") or dest == "system":
        die(f"source {i}: dest {dest!r} is reserved")
    for d in seen:
        if dest == d or dest.startswith(d + "/") or d.startswith(dest + "/"):
            die(f"source {i}: dest {dest!r} overlaps {d!r}")
    seen.append(dest)
    if any(c in v for v in (path, dest) for c in "\t\n\r"):
        die(f"source {i}: tabs and newlines are not allowed in path or dest")
    excl = s.get("excludes", [])
    if not isinstance(excl, list) or not all(isinstance(e, str) and e and "\n" not in e and "\r" not in e for e in excl):
        die(f"source {i}: excludes must be a list of non-empty strings")
    xfile = ""
    if excl:
        xfile = os.path.join(work, f"excludes-{i}")
        with open(xfile, "w") as f:
            f.write("\n".join(excl) + "\n")
    flags = []
    for k, default in (("optional", False), ("vfat", False), ("one_file_system", True)):
        val = s.get(k, default)
        if not isinstance(val, bool):
            die(f"source {i}: {k!r} must be a boolean")
        flags.append(str(int(val)))
    print("\t".join([path, dest, *flags, xfile]))
PYEOF
)" || { log "Invalid config, aborting."; exit 1; }

# ---------------------------------------------------------------- locate the (auto-mounted) drive
# The drive is unlocked and mounted by the desktop (udisks automount, /run/media/<user>/...).
# This script never unlocks or mounts anything itself.
FS_DEV="$(findmnt -lnf --nofsroot -S "LABEL=$FS_LABEL" -o SOURCE 2>/dev/null || true)"
MOUNT_POINT="$(findmnt -lnf -S "LABEL=$FS_LABEL" -o TARGET 2>/dev/null || true)"
if [[ -z $FS_DEV || -z $MOUNT_POINT ]]; then
  log "Backup drive (btrfs label $FS_LABEL) is not mounted. Plug it in and unlock it in the file manager, then retry."
  exit 1
fi
[[ -f $MOUNT_POINT/$MARKER ]] || { log "Marker $MARKER missing on $MOUNT_POINT: wrong drive. Aborting."; exit 1; }
[[ $(findmnt -M "$MOUNT_POINT" -no FSTYPE -f) == btrfs ]] || { log "Target is not btrfs. Aborting."; exit 1; }

mount_opts="$(findmnt -M "$MOUNT_POINT" -no OPTIONS -f)"
if [[ ",$mount_opts," == *",ro,"* ]]; then
  if (( DRY_RUN )); then
    log "WARNING: $MOUNT_POINT is mounted read-only."
  else
    log "Target $MOUNT_POINT is mounted read-only. Aborting."
    exit 1
  fi
fi

avail_bytes="$(df -B1 --output=avail "$MOUNT_POINT" | tail -n1 | xargs)"
min_bytes=$(( MIN_FREE_GIB * 1024 * 1024 * 1024 ))
if (( avail_bytes < min_bytes )); then
  avail_human="$(df -h --output=avail "$MOUNT_POINT" | tail -n1 | xargs)"
  if (( DRY_RUN )); then
    log "WARNING: less than ${MIN_FREE_GIB} GiB free on $MOUNT_POINT ($avail_human available)."
  else
    log "Less than ${MIN_FREE_GIB} GiB free on $MOUNT_POINT ($avail_human available). Aborting."
    exit 1
  fi
fi
log "Using $FS_DEV mounted at $MOUNT_POINT"

HOST_NAME="$(uname -n)"
DEST="$MOUNT_POINT/$DEST_DIR/$HOST_NAME"
DELETED="$DEST/_deleted/$STAMP"
if (( ! DRY_RUN )); then
  mkdir -p "$DEST/packages" "$DEST/system"
fi

# ---------------------------------------------------------------- rsync wrapper
RSYNC_COMMON=(-aAXH --numeric-ids --delete --delete-excluded --human-readable)
(( DRY_RUN )) && RSYNC_COMMON+=(--dry-run --itemize-changes)
(( VERBOSE )) && RSYNC_COMMON+=(-v)

run_rsync() {  # run_rsync <label> <args...>; exit codes 0 and 24 are success
  local label=$1; shift
  log "rsync: $label"
  local rc=0
  rsync "$@" || rc=$?
  if (( rc == 24 )); then
    log "rsync: $label (some source files vanished during transfer, exit 24)"
  elif (( rc != 0 )); then
    log "rsync failed for $label (exit $rc)"
    exit "$rc"
  fi
}

# ---------------------------------------------------------------- 1. package lists
PK="$DEST/packages"
if (( ! DRY_RUN )); then
  log "Dumping package lists"
  pacman -Qqen > "$PK/pacman-native.txt"
  pacman -Qqem > "$PK/pacman-aur.txt" || true
  pacman -Qqe  > "$PK/pacman-all-explicit.txt"
  pacman -Qqd  > "$PK/pacman-deps.txt"
  pacman -Q    > "$PK/pacman-versions.txt"
  LC_ALL=C pacman -Qii 2>/dev/null | awk '/^Name/{pkg=$3} /\[modified\]/{print pkg "\t" $(NF-1)}' \
    > "$PK/pacman-modified-files.txt" || true
  if command -v flatpak >/dev/null; then
    sudo -u "$USER_NAME" flatpak list --app --columns=application,origin > "$PK/flatpak-apps.txt" || true
    sudo -u "$USER_NAME" flatpak remotes --columns=name,url > "$PK/flatpak-remotes.txt" || true
  fi
  { omarchy version 2>/dev/null || true; } > "$PK/omarchy-version.txt"
  systemctl list-unit-files --state=enabled > "$PK/systemd-enabled.txt" || true
  sudo -u "$USER_NAME" XDG_RUNTIME_DIR="/run/user/$USER_UID" systemctl --user list-unit-files --state=enabled \
    > "$PK/systemd-user-enabled.txt" 2>/dev/null || true
fi

# ---------------------------------------------------------------- 2. LUKS header
# LUKS header of the root disk (kept only on the encrypted USB)
detect_root_luks_dev() {
  local root_src dev_name dm_name luks_dev=""
  root_src="$(findmnt -no SOURCE --nofsroot / 2>/dev/null || true)"
  if [[ -n $root_src ]]; then
    dev_name="$(basename "$(readlink -f "$root_src" 2>/dev/null)" 2>/dev/null || true)"
    if [[ -n $dev_name && -r /sys/class/block/$dev_name/dm/name ]]; then
      dm_name="$(cat "/sys/class/block/$dev_name/dm/name")"
      luks_dev="$(cryptsetup status "$dm_name" 2>/dev/null | awk '/device:/{print $2}' || true)"
    fi
  fi
  if [[ -z $luks_dev ]]; then
    luks_dev="$(cryptsetup status root 2>/dev/null | awk '/device:/{print $2}' || true)"
  fi
  printf '%s\n' "$luks_dev"
}

if (( ! DRY_RUN )); then
  ROOT_LUKS_DEV="$(detect_root_luks_dev)"
  if [[ -n $ROOT_LUKS_DEV ]]; then
    hdr_name="luks-header-$(basename "$ROOT_LUKS_DEV").img"
    hdr="$DEST/system/$hdr_name"
    hdr_tmp="$DEST/system/.$hdr_name.tmp"
    rm -f "$hdr_tmp"
    if cryptsetup luksHeaderBackup "$ROOT_LUKS_DEV" --header-backup-file "$hdr_tmp"; then
      chmod 600 "$hdr_tmp"
      mv -f "$hdr_tmp" "$hdr"
    else
      rm -f "$hdr_tmp"
      log "WARNING: LUKS header backup failed"
    fi
  else
    log "WARNING: could not detect root LUKS device, skipping header backup"
  fi
fi

# ---------------------------------------------------------------- 3. files (sources from the config)
while IFS=$'\t' read -r -u 3 src dest optional vfat one_fs xfile; do
  if [[ ! -e $src ]]; then
    if (( optional )); then log "skip: $src (not present)"; continue; fi
    log "Source $src does not exist. Aborting."; exit 1
  fi
  if [[ ! -d $src ]]; then
    log "Source $src is not a directory. Aborting."; exit 1
  fi
  if (( ! DRY_RUN )); then
    mkdir -p "$DEST/$dest"
  fi
  opts=()
  if (( vfat )); then  # FAT: no ownership, permissions or ACLs
    opts+=(-rt --modify-window=2 --delete --delete-excluded --human-readable)
    (( DRY_RUN )) && opts+=(--dry-run --itemize-changes)
    (( VERBOSE )) && opts+=(-v)
  else
    opts+=("${RSYNC_COMMON[@]}")
    (( DRY_RUN )) || opts+=(--backup --backup-dir="$DELETED/$dest")
  fi
  (( one_fs )) && opts+=(-x)
  [[ -n $xfile ]] && opts+=(--exclude-from="$xfile")
  run_rsync "$src" "${opts[@]}" "$src/" "$DEST/$dest/"
done 3<<< "$SOURCES"

# ---------------------------------------------------------------- 4. finish
if (( ! DRY_RUN )); then
  # prune old deleted/overwritten files
  find "$DEST/_deleted" -mindepth 1 -maxdepth 1 -type d -mtime +"$KEEP_DELETED_DAYS" -exec rm -rf {} + 2>/dev/null || true
  {
    echo "date: $(date -Is)"
    echo "host: $HOST_NAME"
    echo "kernel: $(uname -r)"
  } > "$DEST/LAST_BACKUP"
  sync
  ls -1t "$LOG_DIR"/backup-*.log 2>/dev/null | tail -n +$((KEEP_LOGS + 1)) | xargs -r -d '\n' rm -f
fi

log "Backup finished$( (( DRY_RUN )) && echo ' (dry run)' ). Free space: $(df -h --output=avail "$MOUNT_POINT" | tail -1 | xargs)"
notify "Backup finished" "$( (( DRY_RUN )) && echo 'Dry run' || echo 'OK' )"
