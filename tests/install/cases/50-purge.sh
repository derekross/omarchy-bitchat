#!/usr/bin/env bash
source "$T/lib.sh"

case_ "1. --purge deletes only bitchat's named files, keeps backups and anything else, lists the backups"
fresh; printf 'old\n' >"$BIN/bitchatd"; inst "--replace-existing=$BIN/bitchatd" >/dev/null
echo '{}' >"$DATA/identity.json"; echo mine >"$DATA/notes.txt"
echo m >"$STATE/messages.jsonl"; echo p >"$STATE/peers.json"
echo s >"$HOME/real-settings"; ln -s "$HOME/real-settings" "$STATE/settings.json"
r=$(uninst --purge)
expect "exit 0" [ "$r" = 0 ] &&
expect "identity gone" [ ! -e "$DATA/identity.json" ] &&
expect "history gone" [ ! -e "$STATE/messages.jsonl" ] && [ ! -e "$STATE/peers.json" ] &&
expect "record gone" [ ! -e "$M" ] && [ ! -e "$INST/.lock" ] && expect "install folder kept (backups)" [ -d "$B" ] &&
expect "foreign file kept" [ -f "$DATA/notes.txt" ] && said "kept $DATA" &&
expect "link not removed or followed" [ -L "$STATE/settings.json" ] && [ -f "$HOME/real-settings" ] && said "keeping $STATE/settings.json" &&
expect "backup kept" [ "$(backups bitchatd)" = 1 ] && grep -qx old "$B"/bitchatd.* &&
expect "backups listed" said "kept in $B" && said "$B/bitchatd." && ok || unnamed

case_ "2. --purge of a plain install removes both folders"
fresh; inst >/dev/null; echo '{}' >"$DATA/identity.json"; echo s >"$STATE/settings.json"; r=$(uninst --purge)
expect "exit 0" [ "$r" = 0 ] && expect "data dir gone" [ ! -e "$DATA" ] && expect "state dir gone" [ ! -e "$STATE" ] && expect "install dir gone" [ ! -e "$INST" ] && ok || unnamed

case_ "3. --purge is refused while the service keeps running under a unit this won't stop"
fresh; inst >/dev/null; echo '{}' >"$DATA/identity.json"; echo "# mine" >>"$U"; : >"$FAKE_LOG"
r=$(FAKE_ACTIVE_RC=0 uninst --purge)
expect "exit 1" [ "$r" = 1 ] && said "Not purging" && said "Nothing was deleted" &&
expect "identity kept" [ -f "$DATA/identity.json" ] && expect "binaries kept" [ -f "$BIN/bitchatd" ] &&
expect "unit kept" [ -f "$U" ] && expect "not stopped" not_logged "--user stop" && ok || unnamed

case_ "4. --purge stops bitchat's own unit, but still won't delete if it keeps running"
fresh; inst >/dev/null; echo '{}' >"$DATA/identity.json"; : >"$FAKE_LOG"
r=$(FAKE_ACTIVE_RC=0 uninst --purge)
expect "exit 1" [ "$r" = 1 ] && expect "tried to stop it" logged "disable --now bitchat.service" &&
said "still running" && expect "identity kept" [ -f "$DATA/identity.json" ] && ok || unnamed

finish
