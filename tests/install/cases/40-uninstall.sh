#!/usr/bin/env bash
source "$T/lib.sh"

case_ "1. uninstall after install: disables and removes what it wrote, unlinks the plugin, keeps data"
fresh; inst >/dev/null; echo '{}' >"$DATA/identity.json"; : >"$FAKE_LOG"; r=$(uninst)
expect "exit 0" [ "$r" = 0 ] &&
expect "disabled" logged "disable --now bitchat.service" &&
expect "binaries gone" [ ! -e "$BIN/bitchatd" ] && [ ! -e "$BIN/bitchatctl" ] &&
expect "unit gone" [ ! -e "$U" ] &&
expect "reloaded" logged "daemon-reload" &&
expect "link gone" [ ! -e "$P" ] && [ ! -L "$P" ] &&
expect "plugin disabled" logged "omarchy plugin disable derekross.bitchat" &&
expect "data kept" [ -f "$DATA/identity.json" ] && said "Kept your identity" &&
expect "no hash lines left" [ -z "$(grep -v '^backup' "$M" 2>/dev/null)" ] &&
expect "no temp files" no_temps && ok || unnamed

case_ "2. only what it owns: a foreign binary and a link stay"
fresh; inst >/dev/null
printf 'theirs\n' >"$BIN/bitchatctl"; rm "$BIN/bitchatd"; ln -s /bin/true "$BIN/bitchatd"; r=$(uninst)
expect "exit 0" [ "$r" = 0 ] &&
expect "foreign kept" [ "$(cat "$BIN/bitchatctl")" = theirs ] && said "keeping $BIN/bitchatctl: bitchat has no record" &&
expect "link kept" [ -L "$BIN/bitchatd" ] && said "keeping $BIN/bitchatd: it's a symbolic link" &&
expect "own unit removed" [ ! -e "$U" ] && ok || unnamed

case_ "3. a plugin path that isn't this checkout is kept, and the plugin isn't disabled"
fresh; mkdir -p "$(dirname "$P")"; ln -s "$HOME" "$P"; inst >/dev/null; : >"$FAKE_LOG"; r=$(uninst)
expect "kept" [ -L "$P" ] && [ "$(readlink "$P")" = "$HOME" ] && said "Keeping $P" && expect "not disabled" not_logged "plugin disable" && ok || unnamed

case_ "4. bitchat's unit set (by a drop-in) to start something else: not stopped, kept, explained"
fresh; inst >/dev/null; : >"$FAKE_LOG"
r=$(FAKE_EXECSTART="{ path=/opt/other ; argv[]=/opt/other }" uninst)
expect "kept" [ -f "$U" ] && expect "not stopped" not_logged "--user disable" && not_logged "--user stop" && said "start something other than" && ok || unnamed

case_ "5. unit never loaded by systemd: removed without systemctl stop/disable"
fresh; inst >/dev/null; : >"$FAKE_LOG"; r=$(FAKE_FRAGMENT="" uninst)
expect "removed" [ ! -e "$U" ] && expect "no disable" not_logged "--user disable" && ok || unnamed

case_ "6. the runtime folder: only the socket and lock by name, only when the service is stopped"
fresh; inst >/dev/null; R="$XDG_RUNTIME_DIR/bitchat"; mkdir -m 700 "$R"; echo 1 >"$R/bitchat.lock"
if make_socket "$R/bitchat.sock" && make_socket "$XDG_RUNTIME_DIR/bitchat.sock"; then
  r=$(FAKE_ACTIVE_RC=0 FAKE_EXECSTART="{ path=/opt/other ; argv[]=/opt/other }" uninst)
  expect "kept while running" [ -S "$R/bitchat.sock" ] && said "still running" && ok || unnamed
  r=$(uninst)
  expect "socket removed" [ ! -e "$R/bitchat.sock" ] && expect "lock removed" [ ! -e "$R/bitchat.lock" ] &&
  expect "folder removed" [ ! -e "$R" ] && expect "old socket removed" [ ! -e "$XDG_RUNTIME_DIR/bitchat.sock" ] && ok || unnamed
else echo "   ok (skipped: no python3 to make a socket)"; fi
fresh; inst >/dev/null; R="$XDG_RUNTIME_DIR/bitchat"; mkdir -m 700 "$R"
echo data >"$R/bitchat.sock"; echo mine >"$R/other"; echo data >"$XDG_RUNTIME_DIR/bitchat.sock"; r=$(uninst)
expect "a regular file at the socket path stays" [ -f "$R/bitchat.sock" ] && [ -f "$XDG_RUNTIME_DIR/bitchat.sock" ] &&
expect "folder with other files stays" [ -f "$R/other" ] && ok || unnamed
fresh; inst >/dev/null; mkdir "$HOME/elsewhere"; echo 1 >"$HOME/elsewhere/bitchat.lock"; ln -s "$HOME/elsewhere" "$XDG_RUNTIME_DIR/bitchat"; r=$(uninst)
expect "a linked runtime folder isn't followed" [ -f "$HOME/elsewhere/bitchat.lock" ] && [ -L "$XDG_RUNTIME_DIR/bitchat" ] && ok || unnamed

case_ "7. uninstall twice is harmless"
r=$(uninst); expect "exit 0" [ "$r" = 0 ] && ok || unnamed

finish
