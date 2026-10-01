#!/usr/bin/env bash
# Remove what dist/install.sh installed: stop and disable bitchat.service,
# remove bitchatd, bitchatctl and the unit (only files whose SHA-256 matches
# the install record, or the unit carrying our marker line), and the plugin
# link if it points at this checkout.
#
#   ./dist/uninstall.sh           keep your identity and chat history
#   ./dist/uninstall.sh --purge   also delete them (your peer ID changes)
set -euo pipefail

cd "$(dirname "$0")/.."
REPO="$(pwd -P)"
PLUGIN_ID="derekross.bitchat"
BINDIR="$HOME/.local/bin"
UNIT="bitchat.service"
UNITDIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
UNIT_MARKER="# Installed by omarchy-bitchat"
PLUGIN_PATH="$HOME/.config/omarchy/plugins/$PLUGIN_ID"
# Fixed, matching the unit (dist/bitchat.service).
STATEDIR="$HOME/.local/state/omarchy-bitchat"
DATADIR="$HOME/.local/share/omarchy-bitchat"
RECORD="$STATEDIR/installed.tsv"

PURGE=0
for arg in "$@"; do
  case $arg in
    --purge) PURGE=1 ;;
    -h | --help) sed -n '2,9p' "$0"; exit 0 ;;
    *) echo "Unknown option: $arg (see --help)" >&2; exit 2 ;;
  esac
done

sha() { sha256sum -- "$1" | cut -d' ' -f1; }
recorded_sha() { [[ -f "$RECORD" && ! -L "$RECORD" ]] && awk -F'\t' -v p="$1" '$2 == p { print $1 }' "$RECORD" | tail -1 || true; }

if systemctl --user list-unit-files "$UNIT" >/dev/null 2>&1; then
  systemctl --user disable --now "$UNIT" 2>/dev/null || true
fi

remove_if_ours() {
  local dest="$1"
  if [[ -L $dest || ! -e $dest ]]; then return; fi
  if [[ "$(sha "$dest")" == "$(recorded_sha "$dest")" ]] \
    || { [[ $dest == *.service ]] && head -n1 -- "$dest" | grep -qF "$UNIT_MARKER"; }; then
    rm -f -- "$dest"
    echo "Removed $dest"
  else
    echo "Left $dest alone: it isn't the file this plugin installed."
  fi
}

remove_if_ours "$BINDIR/bitchatd"
remove_if_ours "$BINDIR/bitchatctl"
remove_if_ours "$UNITDIR/$UNIT"
systemctl --user daemon-reload 2>/dev/null || true

if [[ -L "$PLUGIN_PATH" && "$(realpath -m "$PLUGIN_PATH")" == "$REPO" ]]; then
  if command -v omarchy >/dev/null; then omarchy plugin disable "$PLUGIN_ID" >/dev/null 2>&1 || true; fi
  rm -f -- "$PLUGIN_PATH"
  echo "Removed the plugin link $PLUGIN_PATH"
fi

rm -f -- "${XDG_RUNTIME_DIR:-/nonexistent}/bitchat.sock"

if (( PURGE )); then
  rm -rf -- "$DATADIR" "$STATEDIR"
  echo "Deleted your bitchat identity and history."
else
  rm -f -- "$RECORD"
  echo "Kept your identity ($DATADIR) and history ($STATEDIR). --purge deletes them."
fi
