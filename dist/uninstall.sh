#!/usr/bin/env bash
# Remove what dist/install.sh installed: bitchatd, bitchatctl, the
# bitchat.service unit, and the plugin link if it points at this checkout.
#
#   ./dist/uninstall.sh           keep your identity and chat history
#   ./dist/uninstall.sh --purge   also delete them (your peer ID changes)
#
# Only what bitchat can prove it wrote is removed (rule in dist/lib.sh): a
# unit you edited, a masked or linked unit, and files bitchat has no record
# of stay where they are, and the output says why. Nothing is removed
# recursively, and files moved aside with --replace-existing (in
# ~/.local/state/omarchy-bitchat-install/backup/, or .../omarchy-bitchat/backup/
# from older versions) are never deleted, not even by --purge. --purge is
# refused while the service keeps running (a unit this can't stop).
set -euo pipefail

cd "$(dirname "$0")/.."
REPO="$(pwd -P)"
[[ -f dist/lib.sh ]] || { echo "Run this from an omarchy-bitchat checkout." >&2; exit 1; }
source dist/lib.sh

PURGE=0
for arg in "$@"; do
  case $arg in
    --purge) PURGE=1 ;;
    -h | --help) sed -n '2,12p' "$0"; exit 0 ;;
    *) echo "Unknown option: $arg (see --help)" >&2; exit 2 ;;
  esac
done

prepare_state
load_record
load_known

# ── The service ────────────────────────────────────────────────────────
inspect_unit
# Only bitchat's own unit, starting bitchat's binary, is stopped here; if
# anything else keeps the daemon running, its files aren't deleted under it.
WILL_STOP=0
[[ $UNIT_STATE == owned && -n $UNIT_FRAGMENT ]] && unit_runs_our_binary && WILL_STOP=1
if (( PURGE && ! WILL_STOP )) && unit_is_active; then
  say "Not purging: $SERVICE is running and this won't stop it (the unit isn't bitchat's own, or doesn't start $BINDIR/bitchatd). Nothing was deleted."
  note "Stop it first (systemctl --user disable --now $SERVICE), then run this again."
  exit 1
fi
say "Stopping and removing $SERVICE"
STOPPED=0
case $UNIT_STATE in
  owned)
    if [[ -z $UNIT_FRAGMENT ]]; then
      # systemd doesn't have it loaded: nothing to stop or disable.
      remove_owned "$UNIT" "$OWN_HASH" || true
    elif unit_runs_our_binary; then
      systemctl_user disable --now "$SERVICE" && STOPPED=1
      remove_owned "$UNIT" "$OWN_HASH" || true
    else
      note "$SERVICE is bitchat's unit, but it is set (by a drop-in?) to start something other than $BINDIR/bitchatd,"
      note "so it is left running and kept. When you're done with it: systemctl --user disable --now $SERVICE && rm $UNIT"
    fi ;;
  edited | foreign)
    if [[ $UNIT_STATE == edited ]]; then note "keeping $UNIT: you changed it, so it isn't bitchat's to remove."
    else note "keeping $UNIT: bitchat has no record of writing it."; fi
    if unit_runs_our_binary; then
      note "It starts $BINDIR/bitchatd, which this removes. Stop it and remove it yourself:"
      note "  systemctl --user disable --now $SERVICE && rm $UNIT"
    fi ;;
  masked | symlink | elsewhere | other) note "$(unit_refusal)" ;;
  missing) ;;
esac
(( UNIT_DROPIN )) && note "$UNIT.d/ drop-ins are yours; not touched."
systemctl_user daemon-reload || true

# ── Binaries ───────────────────────────────────────────────────────────
say "Removing binaries"
for b in "${BINARIES[@]}"; do
  p="$BINDIR/$b"
  classify "$p" "bin/$b"
  case $FILE_STATE in
    owned) remove_owned "$p" "$OWN_HASH" || true ;;
    foreign) note "keeping $p: bitchat has no record of writing it ($(describe_file "$p"))." ;;
    symlink) note "keeping $p: it's a symbolic link, never bitchat's." ;;
    other) note "keeping $p: it isn't a regular file." ;;
    missing) ;;
  esac
done

# ── The plugin link ────────────────────────────────────────────────────
if plugin_link_is_ours; then
  command -v omarchy >/dev/null && { omarchy plugin disable "$PLUGIN_ID" >/dev/null 2>&1 || true; }
  rm -f -- "$PLUGIN_PATH"
  say "Removed the plugin link $PLUGIN_PATH"
  command -v omarchy-shell >/dev/null && { omarchy-shell shell rescanPlugins >/dev/null 2>&1 || true; }
elif this_checkout_is_plugin_dir; then
  say "The plugin is this checkout ($PLUGIN_PATH); remove it with: omarchy plugin remove $PLUGIN_ID"
elif [[ -e $PLUGIN_PATH || -L $PLUGIN_PATH ]]; then
  say "Keeping $PLUGIN_PATH: it isn't a link to this checkout."
fi

# ── The control socket ─────────────────────────────────────────────────
# Only a socket, and the daemon's lock file, by name; never while it runs.
if [[ -n $SOCKDIR ]]; then
  if unit_is_active; then
    if [[ -S $SOCKDIR/bitchat.sock ]]; then note "keeping $SOCKDIR/bitchat.sock: $SERVICE is still running."; fi
  else
    if [[ -d $SOCKDIR && ! -L $SOCKDIR ]]; then
      [[ -S $SOCKDIR/bitchat.sock && ! -L $SOCKDIR/bitchat.sock ]] && rm -f -- "$SOCKDIR/bitchat.sock"
      [[ -f $SOCKDIR/bitchat.lock && ! -L $SOCKDIR/bitchat.lock ]] && rm -f -- "$SOCKDIR/bitchat.lock"
      rmdir -- "$SOCKDIR" 2>/dev/null || true
    fi
    if [[ -S $OLD_SOCKET && ! -L $OLD_SOCKET ]]; then rm -f -- "$OLD_SOCKET"; fi
  fi
fi

# ── The record ─────────────────────────────────────────────────────────
# Lines only for files still there and still the recorded bytes; backup
# lines stay, so their files can always be found.
for p in "${!RECORD_HASH[@]}"; do
  h="$(file_hash "$p" || true)"
  [[ -n $h && $h == "${RECORD_HASH[$p]}" ]] || unset 'RECORD_HASH[$p]'
done
write_record

# ── --purge ────────────────────────────────────────────────────────────
if (( PURGE )) && unit_is_active; then
  say
  say "Not purging: $SERVICE is still running (stopping it failed, see above). Your identity and history are kept."
  list_backups
  exit 1
elif (( PURGE )); then
  say "Deleting your identity and history"
  for f in "$DATADIR/identity.json" \
           "$STATEDIR/messages.jsonl" "$STATEDIR/settings.json" "$STATEDIR/peers.json" \
           "$RECORD" "$LOCKFILE"; do
    case "$(path_kind "$f")" in
      file) rm -f -- "$f" && note "removed $f" ;;
      missing) ;;
      *) note "keeping $f: it isn't a regular file." ;;
    esac
  done
  for d in "$DATADIR" "$STATEDIR" "$INSTDIR"; do
    [[ -d $d && ! -L $d ]] || continue
    rmdir -- "$d" 2>/dev/null || note "kept $d: it still holds files that aren't bitchat's to delete."
  done
else
  say
  say "Kept your identity ($DATADIR) and history ($STATEDIR). --purge deletes them."
fi
list_backups
say "bitchat removed."
