#!/usr/bin/env bash
# test.sh - lightweight syntax and unit smoke tests for omarchy-backup.
set -euo pipefail

REPO_DIR="$(dirname "$(readlink -f "$0")")"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

pass=0
fail=0
ok()   { pass=$((pass + 1)); printf 'PASS: %s\n' "$1"; }
err()  { fail=$((fail + 1)); printf 'FAIL: %s\n' "$1" >&2; }

# 1. Shell syntax checks
for script in omarchy-backup.sh format-usb.sh test.sh; do
  if bash -n "$REPO_DIR/$script"; then
    ok "bash -n $script"
  else
    err "bash -n $script"
  fi
done

# 2. Label consistency between format-usb.sh and omarchy-backup.sh
fmt_label="$(awk -F= '/^FS_LABEL=/{gsub(/"/, "", $2); print $2}' "$REPO_DIR/format-usb.sh")"
bak_label="$(awk -F= '/^FS_LABEL=/{gsub(/[[:space:]]*#.*/, "", $2); gsub(/"/, "", $2); print $2}' "$REPO_DIR/omarchy-backup.sh")"
if [[ -n $fmt_label && $fmt_label == "$bak_label" ]]; then
  ok "FS_LABEL matches across format-usb.sh and omarchy-backup.sh ($fmt_label)"
else
  err "FS_LABEL mismatch: format-usb.sh='$fmt_label' vs omarchy-backup.sh='$bak_label'"
fi

# 3. CLI option handling (non-root safe checks)
if "$REPO_DIR/omarchy-backup.sh" --help >/dev/null 2>&1; then
  ok "omarchy-backup.sh --help exits 0"
else
  err "omarchy-backup.sh --help"
fi
rc=0; "$REPO_DIR/omarchy-backup.sh" --not-a-real-flag >/dev/null 2>&1 || rc=$?
if (( rc == 2 )); then
  ok "omarchy-backup.sh rejects unknown flag with exit 2"
else
  err "omarchy-backup.sh unknown flag exited $rc (expected 2)"
fi
rc=0; "$REPO_DIR/omarchy-backup.sh" --config >/dev/null 2>&1 || rc=$?
if (( rc == 2 )); then
  ok "omarchy-backup.sh rejects missing --config argument with exit 2"
else
  err "omarchy-backup.sh missing --config arg exited $rc (expected 2)"
fi

# 4. Extract and test the embedded Python TOML config loader
LOADER="$WORK/loader.py"
awk '/<<'\''PYEOF'\''/{flag=1; next} /^PYEOF$/{flag=0} flag' "$REPO_DIR/omarchy-backup.sh" > "$LOADER"
[[ -s $LOADER ]] || { err "could not extract Python loader from omarchy-backup.sh"; exit 1; }

run_loader() {
  local cfg="$1" out_work="$WORK/sub-$RANDOM"
  mkdir -p "$out_work"
  python3 "$LOADER" "$cfg" "/home/testuser" "$out_work"
}

# 4a. Production config parses cleanly
if out="$(run_loader "$REPO_DIR/omarchy-backup.toml")"; then
  home_line="$(printf '%s\n' "$out" | awk -F'\t' '$2 == "home"')"
  IFS=$'\t' read -r h_path h_dest h_opt h_vfat h_onefs h_xfile <<< "$home_line"
  if [[ $h_path == "/home/testuser" && $h_dest == "home" && $h_opt == 0 && $h_vfat == 0 && $h_onefs == 1 && -f $h_xfile ]] \
     && grep -qx '/.local/state/omarchy-backup/' "$h_xfile"; then
    ok "production omarchy-backup.toml parses with expected fields and excludes"
  else
    err "unexpected parsed home line: $home_line"
  fi
else
  err "production omarchy-backup.toml failed to parse"
fi

# 4b. Explicit one_file_system = false works
cat > "$WORK/onefs-false.toml" <<'EOF'
[[source]]
path = "/etc"
dest = "system/etc"
one_file_system = false
EOF
if out="$(run_loader "$WORK/onefs-false.toml")" && [[ $(printf '%s\n' "$out" | cut -f5) == "0" ]]; then
  ok "one_file_system = false parsed as 0"
else
  err "one_file_system = false failed"
fi

# 4c. Invalid configs must be rejected
expect_reject() {
  local name="$1" content="$2"
  printf '%s\n' "$content" > "$WORK/bad.toml"
  if run_loader "$WORK/bad.toml" >/dev/null 2>&1; then
    err "expected rejection for: $name"
  else
    ok "rejected invalid config: $name"
  fi
}

expect_reject 'string boolean optional="false"' '
[[source]]
path = "/etc"
dest = "system/etc"
optional = "false"
'
expect_reject 'string boolean one_file_system="true"' '
[[source]]
path = "/etc"
dest = "system/etc"
one_file_system = "true"
'
expect_reject 'non-table source entry' '
source = ["not-a-table"]
'
expect_reject 'reserved dest LAST_BACKUP' '
[[source]]
path = "/etc"
dest = "LAST_BACKUP"
'
expect_reject 'reserved exact dest system' '
[[source]]
path = "/etc"
dest = "system"
'
expect_reject 'overlapping dests' '
[[source]]
path = "/etc"
dest = "a"

[[source]]
path = "/usr/local"
dest = "a/b"
'
expect_reject 'empty exclude string' '
[[source]]
path = "/etc"
dest = "system/etc"
excludes = [""]
'
expect_reject 'no sources' '
'

# 5. pacman-modified-files.txt awk parser
sample_qii=$'Name            : filesystem\nBackup Files    : /etc/fstab [modified]\n                  /etc/group [modified]\n                  /etc/hosts [unmodified]\nName            : pacman\nBackup Files    : /etc/pacman.conf [unmodified]\n'
parsed_mod="$(printf '%s' "$sample_qii" | awk '/^Name/{pkg=$3} /\[modified\]/{print pkg "\t" $(NF-1)}')"
expected_mod=$'filesystem\t/etc/fstab\nfilesystem\t/etc/group'
if [[ $parsed_mod == "$expected_mod" ]]; then
  ok "pacman -Qii [modified] awk filter extracts modified files only"
else
  err "pacman -Qii filter mismatch: got '$parsed_mod'"
fi

printf '\nSummary: %d passed, %d failed\n' "$pass" "$fail"
(( fail == 0 ))
