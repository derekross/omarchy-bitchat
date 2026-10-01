#!/usr/bin/env bash
source "$T/lib.sh"

case_ "1. a symlinked state folder, data folder, record or backup folder is refused"
fresh; mkdir -p "$HOME/elsewhere" "$HOME/.local/state"; ln -s "$HOME/elsewhere" "$STATE"; r=$(inst)
expect "install refused" [ "$r" = 1 ] && said "is a symbolic link" && expect "nothing in the target" [ -z "$(ls -A "$HOME/elsewhere")" ] && ok || unnamed
r=$(uninst); expect "uninstall refused" [ "$r" = 1 ] && ok || unnamed
fresh; mkdir -p "$HOME/elsewhere" "$HOME/.local/share"; ln -s "$HOME/elsewhere" "$DATA"; r=$(inst)
expect "data link refused" [ "$r" = 1 ] && [ ! -e "$BIN/bitchatd" ] && ok || unnamed
fresh; mkdir -p "$INST"; echo x >"$HOME/rec"; ln -s "$HOME/rec" "$M"; r=$(inst)
expect "record link refused" [ "$r" = 1 ] && [ "$(cat "$HOME/rec")" = x ] && ok || unnamed
fresh; mkdir -p "$INST" "$HOME/elsewhere"; ln -s "$HOME/elsewhere" "$B"; r=$(inst)
expect "backup link refused" [ "$r" = 1 ] && ok || unnamed
fresh; mkdir -p "$INST"; ln -s "$HOME/nothing" "$INST/.lock"; r=$(inst)
expect "lock link refused" [ "$r" = 1 ] && [ ! -e "$HOME/nothing" ] && ok || unnamed

case_ "2. two runs can't interleave"
fresh; mkdir -p "$INST"; : >"$INST/.lock"
flock "$INST/.lock" -c "cd '$ROOT' && ./dist/install.sh --no-build </dev/null >'$HOME/out.txt' 2>&1"; r=$?
expect "refused while locked" [ "$r" = 1 ] && said "another bitchat install or uninstall is running" && expect "nothing written" [ ! -e "$BIN/bitchatd" ] && ok || unnamed

case_ "3. a record line for a path the scripts don't write is ignored"
fresh; inst >/dev/null; echo victim >"$HOME/victim"
printf '%s\t%s\n' "$(sha256sum <"$HOME/victim" | cut -d' ' -f1)" "$HOME/victim" >>"$M"; r=$(uninst)
expect "exit 0" [ "$r" = 0 ] && expect "untouched" [ -f "$HOME/victim" ] && said "isn't a path this script writes" && ok || unnamed

finish
