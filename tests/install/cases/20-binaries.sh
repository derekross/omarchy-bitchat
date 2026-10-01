#!/usr/bin/env bash
source "$T/lib.sh"

case_ "1. a binary bitchat has no record of: refused before the build, nothing written"
fresh; printf 'someone else\n' >"$BIN/bitchatd"; cp "$BIN/bitchatd" "$HOME/orig"; r=$(inst)
expect "exit 1" [ "$r" = 1 ] &&
expect "names it" said "$BIN/bitchatd exists but bitchat has no record of writing it" &&
expect "names the flag" said "--replace-existing=$BIN/bitchatd" &&
expect "untouched" same "$HOME/orig" "$BIN/bitchatd" &&
expect "no build" not_logged "cargo" &&
expect "nothing else written" [ ! -e "$BIN/bitchatctl" ] && [ ! -e "$U" ] && [ ! -e "$P" ] && [ ! -e "$M" ] &&
expect "no systemctl writes" not_logged "enable" && not_logged "daemon-reload" && ok || unnamed

case_ "2. --replace-existing=<path>: moved to a backup (recorded, never deleted), then installed"
r=$(inst "--replace-existing=$BIN/bitchatd")
expect "exit 0" [ "$r" = 0 ] &&
expect "new binary" same "$FAKE_BINS/bitchatd" "$BIN/bitchatd" &&
expect "one backup" [ "$(backups bitchatd)" = 1 ] &&
expect "backup content" same "$HOME/orig" "$B"/bitchatd.* &&
expect "backup recorded" grep -q "^backup	$BIN/bitchatd	$B/bitchatd\." "$M" &&
expect "said where" said "moved the previous $BIN/bitchatd to $B/bitchatd." &&
expect "record verifies" record_verifies && ok || unnamed
r=$(inst --no-build "--replace-existing=$BIN/bitchatd")
expect "consent unused on an owned file: no second backup" [ "$(backups bitchatd)" = 1 ] && ok || unnamed

case_ "3. consent is per path"
fresh; printf 'a\n' >"$BIN/bitchatd"; printf 'b\n' >"$BIN/bitchatctl"; r=$(inst "--replace-existing=$BIN/bitchatd")
expect "stops on the other" [ "$r" = 1 ] && said "$BIN/bitchatctl exists but bitchat has no record" &&
expect "the consented one untouched too" [ "$(cat "$BIN/bitchatd")" = a ] && ok || unnamed

case_ "4. a file that can't be read is never ours (no record, or a record line for it)"
if [ "$(id -u)" = 0 ]; then echo "   ok (skipped: root can read anything)"; else
fresh; printf 'secret\n' >"$BIN/bitchatd"; chmod 000 "$BIN/bitchatd"; r=$(inst)
expect "refused" [ "$r" = 1 ] && said "can't be read" && expect "untouched" [ "$(mode "$BIN/bitchatd")" = 0 ] && ok || unnamed
fresh; inst >/dev/null; chmod 000 "$BIN/bitchatctl"; r=$(inst --no-build)
expect "recorded but unreadable: refused" [ "$r" = 1 ] && said "$BIN/bitchatctl exists and can't be read" &&
expect "still there" [ -e "$BIN/bitchatctl" ] && [ "$(mode "$BIN/bitchatctl")" = 0 ] && ok || unnamed
r=$(uninst)
expect "uninstall keeps it" [ -e "$BIN/bitchatctl" ] && said "keeping $BIN/bitchatctl" && expect "removes the readable one" [ ! -e "$BIN/bitchatd" ] && ok || unnamed
chmod 600 "$BIN/bitchatctl"
fi

case_ "5. a symlink or a folder at a binary path: refused, never followed"
fresh; printf 'target\n' >"$HOME/real"; ln -s "$HOME/real" "$BIN/bitchatd"; r=$(inst)
expect "exit 1" [ "$r" = 1 ] && said "is a symbolic link" &&
expect "target untouched" [ "$(cat "$HOME/real")" = target ] && expect "still a link" [ -L "$BIN/bitchatd" ] && ok || unnamed
r=$(inst "--replace-existing=$BIN/bitchatd")
expect "consent doesn't cover links" [ "$r" = 1 ] && [ -L "$BIN/bitchatd" ] && ok || unnamed
fresh; inst >/dev/null; mv "$BIN/bitchatd" "$HOME/moved"; cp "$HOME/moved" "$HOME/real"; ln -s "$HOME/real" "$BIN/bitchatd"; r=$(inst --no-build)
expect "a link to an identical copy is still refused" [ "$r" = 1 ] && [ -L "$BIN/bitchatd" ] && ok || unnamed
fresh; mkdir "$BIN/bitchatctl"; r=$(inst)
expect "folder: exit 1" [ "$r" = 1 ] && said "isn't a regular file" && ok || unnamed

case_ "6. a file that changes after the check is kept as a backup, not overwritten"
fresh; inst >/dev/null
printf 'edited after check\n' >>"$BIN/bitchatctl"
(cd "$ROOT" && REPO=$ROOT && source dist/lib.sh && prepare_state && load_record && replace_owned "$FAKE_BINS/bitchatctl" "$BIN/bitchatctl" 755 "${RECORD_HASH[$BIN/bitchatctl]}" >"$HOME/out.txt" 2>&1)
expect "new file in place" same "$FAKE_BINS/bitchatctl" "$BIN/bitchatctl" &&
expect "old kept" grep -q "edited after check" "$B"/bitchatctl.* &&
expect "said" said "changed after it was checked" &&
expect "backup recorded" grep -q "^backup	$BIN/bitchatctl	" "$M" && ok || unnamed
(cd "$ROOT" && REPO=$ROOT && source dist/lib.sh && prepare_state && load_record && remove_owned "$BIN/bitchatd" "0000000000000000000000000000000000000000000000000000000000000000" >"$HOME/out.txt" 2>&1)
expect "remove: mismatch kept as backup" [ ! -e "$BIN/bitchatd" ] && [ "$(backups bitchatd)" = 1 ] && said "changed after it was checked" && ok || unnamed

case_ "7. a new file never overwrites one that appeared meanwhile"
fresh; printf 'appeared\n' >"$BIN/bitchatd"
(cd "$ROOT" && REPO=$ROOT && source dist/lib.sh && prepare_state && load_record && replace_owned "$FAKE_BINS/bitchatd" "$BIN/bitchatd" 755 "" >"$HOME/out.txt" 2>&1); r=$?
expect "failed" [ "$r" != 0 ] && expect "untouched" [ "$(cat "$BIN/bitchatd")" = appeared ] && said "appeared while installing" &&
expect "not recorded" not record_lists "$BIN/bitchatd" && expect "no temp left" no_temps && ok || unnamed

finish
