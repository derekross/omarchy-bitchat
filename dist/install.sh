#!/usr/bin/env bash
# Build and install Bitchat for the current user: bitchatd and bitchatctl in
# ~/.local/bin, the bitchat.service user unit, and (from a checkout outside
# Omarchy's plugin folder) a link to this checkout in ~/.config/omarchy/plugins.
#
#   ./dist/install.sh                 build with cargo, install, start the daemon
#   ./dist/install.sh --no-build      install already-built binaries
#   ./dist/install.sh --no-start      install but don't enable or start the service
#   ./dist/install.sh --replace-existing
#                                     consent to move aside a file at one of our
#                                     paths that this plugin did not write (it is
#                                     kept as a backup)
#
# Our paths are only ever written when empty or holding a file this plugin
# installed (SHA-256 recorded in $XDG_STATE_HOME/omarchy-bitchat/installed.tsv).
# Anything else is refused, or moved to a backup with --replace-existing.
# No elevated permissions are used.
# Run it again after `git pull` / `omarchy plugin update` to update.
set -euo pipefail

cd "$(dirname "$0")/.."
REPO="$(pwd -P)"
PLUGIN_ID="derekross.bitchat"
BINARIES=(bitchatd bitchatctl)
BINDIR="$HOME/.local/bin"
UNIT="bitchat.service"
UNITDIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
UNIT_MARKER="# Installed by omarchy-bitchat"
PLUGINDIR="$HOME/.config/omarchy/plugins"
PLUGIN_PATH="$PLUGINDIR/$PLUGIN_ID"
# Fixed, matching the unit (dist/bitchat.service).
STATEDIR="$HOME/.local/state/omarchy-bitchat"
DATADIR="$HOME/.local/share/omarchy-bitchat"
RECORD="$STATEDIR/installed.tsv"
BACKUPDIR="$STATEDIR/backup"

BUILD=1
START=1
REPLACE=0
for arg in "$@"; do
  case $arg in
    --no-build) BUILD=0 ;;
    --no-start) START=0 ;;
    --replace-existing) REPLACE=1 ;;
    -h | --help) sed -n '2,18p' "$0"; exit 0 ;;
    *) echo "Unknown option: $arg (see --help)" >&2; exit 2 ;;
  esac
done

[[ -f manifest.json && -d crates/bitchatd ]] || { echo "Run this from an omarchy-bitchat checkout." >&2; exit 1; }

sha() { sha256sum -- "$1" | cut -d' ' -f1; }
recorded_sha() { [[ -f "$RECORD" && ! -L "$RECORD" ]] && awk -F'\t' -v p="$1" '$2 == p { print $1 }' "$RECORD" | tail -1 || true; }

# ── The state folders and record are only ever plain directories and files
#    of ours. A link at any of these paths is refused, never followed.
for p in "$STATEDIR" "$BACKUPDIR" "$DATADIR"; do
  if [[ -L "$p" || (-e "$p" && ! -d "$p") ]]; then
    echo "Refusing to use $p: it is a link or not a directory." >&2; exit 1
  fi
done
if [[ -L "$RECORD" || (-e "$RECORD" && ! -f "$RECORD") ]]; then
  echo "Refusing to use $RECORD: it is a link or not a regular file." >&2; exit 1
fi

# May we write this path? Decided from bytes and the record, never by
# running what is there.
claim() {
  local dest="$1" name
  name="$(basename -- "$dest")"
  if [[ -L $dest ]]; then
    echo "Refusing to write $dest: it is a link. Remove it yourself first." >&2; exit 1
  elif [[ ! -e $dest ]]; then
    return 0
  elif [[ ! -f $dest ]]; then
    echo "Refusing to write $dest: it is not a regular file." >&2; exit 1
  elif [[ "$(sha "$dest")" == "$(recorded_sha "$dest")" ]]; then
    return 0
  elif [[ $dest == *.service ]] && head -n1 -- "$dest" | grep -qF "$UNIT_MARKER"; then
    return 0
  elif (( REPLACE )); then
    mkdir -p "$BACKUPDIR"
    local backup
    backup="$(mktemp -- "$BACKUPDIR/$name.$(date +%Y%m%d-%H%M%S).XXXXXX")"
    mv -f -- "$dest" "$backup"
    echo "Moved the existing $dest (not written by this plugin) to $backup"
  else
    echo "Refusing to overwrite $dest: this plugin has no record of writing it." >&2
    echo "If it is yours to replace, run again with --replace-existing (the file is kept as a backup)." >&2
    exit 1
  fi
}

record() {
  local dest="$1" tmp
  tmp="$(mktemp -- "$STATEDIR/installed.tsv.XXXXXX")"
  { [[ -f "$RECORD" ]] && awk -F'\t' -v p="$dest" '$2 != p' "$RECORD" || true; printf '%s\t%s\n' "$(sha "$dest")" "$dest"; } > "$tmp"
  mv -f -- "$tmp" "$RECORD"
}

for b in "${BINARIES[@]}"; do claim "$BINDIR/$b"; done
claim "$UNITDIR/$UNIT"

# Installed with `omarchy plugin add`, this checkout is the plugin; build
# outside it so the shell doesn't reload on every object file.
if [[ "$REPO" == "$(realpath -m "$PLUGIN_PATH")" ]]; then
  export CARGO_TARGET_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/omarchy-bitchat/target"
fi

if (( BUILD )); then
  command -v cargo >/dev/null || { echo "cargo is needed to build bitchatd: install the Rust toolchain (rustup), then run rustup default stable" >&2; exit 1; }
  pkg-config --exists dbus-1 2>/dev/null || echo "Note: the dbus development files (dbus package) are needed to build; install dbus if the build fails."
  echo "Building bitchatd and bitchatctl…"
  cargo build --release --locked --quiet -p bitchatd -p bitchatctl
fi

TARGET="${CARGO_TARGET_DIR:-$REPO/target}/release"
for b in "${BINARIES[@]}"; do
  [[ -f "$TARGET/$b" && -x "$TARGET/$b" ]] || { echo "No $b at $TARGET/$b; build first" >&2; exit 1; }
done

# The unit's sandbox only lets the daemon write these, so they must exist.
mkdir -p "$BINDIR" "$STATEDIR" "$UNITDIR"
install -d -m700 -- "$DATADIR"
chmod 700 -- "$STATEDIR"

for b in "${BINARIES[@]}"; do
  install -m755 -- "$TARGET/$b" "$BINDIR/$b"
  record "$BINDIR/$b"
  echo "Installed $BINDIR/$b"
done
install -m644 -- dist/$UNIT "$UNITDIR/$UNIT"
record "$UNITDIR/$UNIT"
echo "Installed $UNITDIR/$UNIT"

if [[ ! -e "$PLUGIN_PATH" && ! -L "$PLUGIN_PATH" ]]; then
  mkdir -p "$PLUGINDIR"
  ln -s "$REPO" "$PLUGIN_PATH"
  echo "Linked $PLUGIN_PATH -> $REPO"
elif [[ "$(realpath -m "$PLUGIN_PATH")" != "$REPO" ]]; then
  echo "Note: $PLUGIN_PATH exists and is not this checkout; leaving it alone."
fi

systemctl --user daemon-reload
if (( START )); then
  if systemctl --user is-active --quiet "$UNIT"; then
    systemctl --user restart "$UNIT"
    echo "Restarted $UNIT"
  else
    systemctl --user enable --now "$UNIT"
    echo "Started $UNIT"
  fi
fi

if command -v omarchy-shell >/dev/null; then
  omarchy-shell shell rescanPlugins >/dev/null 2>&1 || true
fi
echo
echo "Now enable it and put it on the bar:"
echo "  omarchy plugin enable $PLUGIN_ID --section right"
echo
echo "bitchatctl status shows what the daemon is doing; journalctl --user -u $UNIT has its log."
