#!/usr/bin/env bash
# Build and install Bitchat for the current user: bitchatd and bitchatctl in
# ~/.local/bin, the bitchat.service user unit, and (from a checkout outside
# Omarchy's plugin folder) a link to this checkout in ~/.config/omarchy/plugins.
#
#   ./dist/install.sh                 build with cargo, install, start the daemon
#   ./dist/install.sh --no-build      install already-built binaries
#   ./dist/install.sh --no-start      install but don't enable, start or restart
#                                     the service
#   --replace-existing=<path>         consent to replace that one file, which
#                                     bitchat has no record of writing (it is
#                                     moved to ~/.local/state/omarchy-bitchat-install/backup/
#                                     and never deleted); repeat for another path
#
# A file is replaced only while it is exactly what bitchat wrote (rule in
# dist/lib.sh); a unit you edited stays and is named; a link or a folder is
# never bitchat's. The service is enabled only on first install; later runs
# restart it only if it is running and starts ~/.local/bin/bitchatd.
# No elevated permissions are used.
# Run it again after `git pull` / `omarchy plugin update` to update.
set -euo pipefail

cd "$(dirname "$0")/.."
REPO="$(pwd -P)"
[[ -f manifest.json && -d crates/bitchatd && -f dist/lib.sh ]] || { echo "Run this from an omarchy-bitchat checkout." >&2; exit 1; }
source dist/lib.sh

BUILD=1 START=1
declare -A CONSENT=()
for arg in "$@"; do
  case $arg in
    --no-build) BUILD=0 ;;
    --no-start) START=0 ;;
    --replace-existing=/*) CONSENT["${arg#--replace-existing=}"]=1 ;;
    --replace-existing | --replace-existing=*)
      die "--replace-existing needs the absolute path of the one file to replace, e.g. --replace-existing=$BINDIR/bitchatd" ;;
    -h | --help) sed -n '2,21p' "$0"; exit 0 ;;
    *) echo "Unknown option: $arg (see --help)" >&2; exit 2 ;;
  esac
done
for p in "${!CONSENT[@]}"; do
  recordable "$p" || die "--replace-existing=$p: not a path this script writes (those are $BINDIR/bitchatd, $BINDIR/bitchatctl and $UNIT)."
done

prepare_state
load_record
load_known

# Installed with `omarchy plugin add`, this checkout *is* the plugin. Build
# outside it: the shell reloads plugins whenever files change in there.
FROM_PLUGIN_CHECKOUT=0
if this_checkout_is_plugin_dir; then
  FROM_PLUGIN_CHECKOUT=1
  export CARGO_TARGET_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/omarchy-bitchat/target"
fi
TARGET="${CARGO_TARGET_DIR:-$REPO/target}/release"

# ── Deciding, per path. Run once before the build (so a refusal costs
#    nothing) and again right before each write (the answer then counts).
ACTION='' WHY=''
foreign_why() {
  local p=$1
  if [[ ! -r $p ]]; then
    WHY="$p exists and can't be read, so bitchat can't tell whether it wrote it."
  else
    WHY="$p exists but bitchat has no record of writing it ($(describe_file "$p"))."
  fi
  WHY+=" To replace it (it is moved to $BACKUPDIR/, never deleted), run again with --replace-existing=$p, or move it aside yourself."
}
decide_bin() {
  local p=$1
  classify "$p" "bin/$(basename -- "$p")"
  case $FILE_STATE in
    missing) ACTION=new ;;
    owned) ACTION=replace ;;
    foreign) if [[ -n ${CONSENT[$p]:-} ]]; then ACTION=backup; else ACTION=stop; foreign_why "$p"; fi ;;
    symlink) ACTION=stop WHY="$p is a symbolic link (to $(readlink -- "$p")); bitchat doesn't follow links. Move it aside, then run this again." ;;
    other) ACTION=stop WHY="$p exists and isn't a regular file. Move it aside, then run this again." ;;
  esac
}
decide_unit() {
  inspect_unit
  case $UNIT_STATE in
    missing) ACTION=new ;;
    owned) ACTION=replace ;;
    edited) if [[ -n ${CONSENT[$UNIT]:-} ]]; then ACTION=backup; else ACTION=keep; fi ;;
    foreign) if [[ -n ${CONSENT[$UNIT]:-} ]]; then ACTION=backup; else ACTION=stop; foreign_why "$UNIT"; fi ;;
    *) ACTION=stop WHY="$(unit_refusal)" ;;
  esac
}

STOPS=()
for b in "${BINARIES[@]}"; do
  decide_bin "$BINDIR/$b"
  [[ $ACTION == stop ]] && STOPS+=("$WHY")
done
decide_unit
[[ $ACTION == stop ]] && STOPS+=("$WHY")
for d in "$BINDIR" "$UNITDIR" "$PLUGINDIR"; do
  [[ -e $d && ! -d $d ]] && STOPS+=("$d exists and isn't a folder. Move it aside, then run this again.")
done
for p in "${!CONSENT[@]}"; do
  if [[ ! -e $p && ! -L $p ]]; then note "--replace-existing=$p: nothing is there, so nothing to replace."; fi
done
if (( ${#STOPS[@]} )); then
  say "Not installing: nothing was changed."
  for s in "${STOPS[@]}"; do note "$s"; done
  exit 1
fi

# ── Build ──────────────────────────────────────────────────────────────
if (( BUILD )); then
  command -v cargo >/dev/null || die "cargo is needed to build bitchatd: install the Rust toolchain (rustup), then run rustup default stable"
  pkg-config --exists dbus-1 2>/dev/null || say "Note: the dbus development files (dbus package) are needed to build; install dbus if the build fails."
  say "Building bitchatd and bitchatctl…"
  cargo build --release --locked --quiet -p bitchatd -p bitchatctl
fi
for b in "${BINARIES[@]}"; do
  [[ -f $TARGET/$b && ! -L $TARGET/$b && -x $TARGET/$b ]] || die "No $b at $TARGET/$b; build first (or drop --no-build). Nothing was changed."
done

# ── Write ──────────────────────────────────────────────────────────────
FAILED=0
# The unit's sandbox only lets the daemon write these, so they must exist.
for d in "$DATADIR" "$STATEDIR"; do
  check_dir "$d"
  [[ -d $d ]] || mkdir -p -m 700 -- "$d" || die "can't create $d"
  check_dir "$d"
  chmod 700 -- "$d"
done

# install_file <src> <dest> <mode>: acts on ACTION/OWN_HASH from the decide_*
# call made just before it.
install_file() {
  local src=$1 dest=$2 mode=$3
  case $ACTION in
    new) replace_owned "$src" "$dest" "$mode" "" && note "installed $dest" ;;
    replace) replace_owned "$src" "$dest" "$mode" "$OWN_HASH" && note "updated $dest" ;;
    backup)
      backup_file "$dest" "$(basename -- "$dest")"
      note "moved the previous $dest to $BACKUP_DEST"
      replace_owned "$src" "$dest" "$mode" "" && note "installed $dest" ;;
    stop) note "$WHY"; note "(it changed since this run started) $dest not installed."; return 1 ;;
  esac
}

say "Installing binaries to $BINDIR"
mkdir -p -- "$BINDIR"
for b in "${BINARIES[@]}"; do
  decide_bin "$BINDIR/$b"
  install_file "$TARGET/$b" "$BINDIR/$b" 755 || FAILED=1
done

say "Installing the systemd user service"
decide_unit
UNIT_ACTION=$ACTION
case $ACTION in
  keep)
    note "keeping your $UNIT: you changed it, so bitchat's version isn't installed. To go back to bitchat's: --replace-existing=$UNIT"
    if [[ -n ${RECORD_HASH[$UNIT]:-} ]]; then unset 'RECORD_HASH[$UNIT]'; write_record; fi ;;
  *) install_file dist/bitchat.service "$UNIT" 644 || FAILED=1 ;;
esac
(( UNIT_DROPIN )) && note "$UNIT.d/ drop-ins are yours; not touched."

# ── The shell plugin ───────────────────────────────────────────────────
PLUGIN_LINKED=0
if (( FROM_PLUGIN_CHECKOUT )); then
  say "Shell plugin: this checkout, installed by 'omarchy plugin add' ($PLUGIN_PATH)"
elif plugin_link_is_ours; then
  say "Shell plugin: $PLUGIN_PATH already links to this checkout"
elif [[ ! -e $PLUGIN_PATH && ! -L $PLUGIN_PATH ]]; then
  mkdir -p -- "$PLUGINDIR"
  if ln -sT -- "$REPO" "$PLUGIN_PATH" 2>/dev/null; then
    say "Linked $PLUGIN_PATH -> $REPO"
    PLUGIN_LINKED=1
  else
    note "$PLUGIN_PATH appeared while installing; left as it is."
  fi
else
  say "Keeping $PLUGIN_PATH: it isn't this checkout, so it's left alone."
fi

if (( FAILED )); then
  say
  say "Some files weren't installed (see above), so the service wasn't started or restarted."
  exit 1
fi

# ── The service ────────────────────────────────────────────────────────
systemctl_user daemon-reload || true
if (( ! START )); then
  say "Not starting $SERVICE (--no-start). Start it with: systemctl --user enable --now $SERVICE"
elif [[ $UNIT_ACTION == new ]]; then
  systemctl_user enable --now "$SERVICE" && say "$SERVICE enabled and started."
else
  inspect_unit
  if unit_is_active && unit_runs_our_binary; then
    systemctl_user restart "$SERVICE" && say "$SERVICE restarted with the new build."
  elif unit_is_active; then
    say "$SERVICE is running but doesn't start $BINDIR/bitchatd; restart it yourself if you want the new build."
  else
    say "$SERVICE isn't running; not started (start it with: systemctl --user enable --now $SERVICE)."
  fi
fi

if (( PLUGIN_LINKED )) && command -v omarchy-shell >/dev/null; then
  omarchy-shell shell rescanPlugins >/dev/null 2>&1 || true
fi
say
say "Now enable it and put it on the bar:"
say "  omarchy plugin enable $PLUGIN_ID --section right"
say
say "bitchatctl status shows what the daemon is doing; journalctl --user -u $SERVICE has its log."
