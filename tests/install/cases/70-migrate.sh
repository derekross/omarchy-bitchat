#!/usr/bin/env bash
source "$T/lib.sh"

# The layout the previous install.sh left: the record inside the daemon's
# state folder, "<sha256>\t<path>" lines.
old_layout() {
  mkdir -p "$(dirname "$U")" "$STATE" "$DATA"; chmod 700 "$STATE" "$DATA"
  cp "$ROOT/dist/bitchat.service" "$U"
  cp "$FAKE_BINS/bitchatd" "$BIN/bitchatd"; cp "$FAKE_BINS/bitchatctl" "$BIN/bitchatctl"
  for f in "$BIN/bitchatd" "$BIN/bitchatctl" "$U"; do printf '%s\t%s\n' "$(sha256sum <"$f" | cut -d' ' -f1)" "$f"; done >"$OLDM"
}

case_ "1. upgrade from a record in the old place: imported once, moved, a plain reinstall"
fresh; old_layout; echo '{}' >"$DATA/identity.json"; r=$(inst)
expect "exit 0" [ "$r" = 0 ] && expect "no refusal" not said "Not installing" &&
expect "said" said "moved the install record from $OLDM to $M" &&
expect "old record gone" [ ! -e "$OLDM" ] && expect "new record verifies" record_verifies &&
expect "lists all three" record_lists "$BIN/bitchatd" && record_lists "$BIN/bitchatctl" && record_lists "$U" &&
expect "no backups" [ ! -e "$B" ] && expect "not enabled again" not_logged "enable" &&
expect "identity untouched" [ -f "$DATA/identity.json" ] && ok || unnamed
r=$(inst --no-build); expect "second run: nothing to import" [ "$r" = 0 ] && not said "moved the install record" && ok || unnamed

case_ "2. an imported line still needs the file on disk to match it"
fresh; old_layout; printf 'replaced by someone\n' >"$BIN/bitchatctl"; r=$(inst)
expect "refused" [ "$r" = 1 ] && said "$BIN/bitchatctl exists but bitchat has no record of writing it" &&
expect "untouched" [ "$(cat "$BIN/bitchatctl")" = "replaced by someone" ] && ok || unnamed
fresh; old_layout; printf 'x\n' >"$HOME/victim"
printf '%s\t%s\n' "$(sha256sum <"$HOME/victim" | cut -d' ' -f1)" "$HOME/victim" >>"$OLDM"; r=$(uninst)
expect "a line for another path is ignored" [ -f "$HOME/victim" ] && said "isn't a path this script writes" &&
expect "the rest removed" [ ! -e "$BIN/bitchatd" ] && [ ! -e "$U" ] && ok || unnamed

case_ "3. a symlinked old record is ignored, not followed, not removed"
fresh; old_layout; mv "$OLDM" "$HOME/elsewhere.tsv"; ln -s "$HOME/elsewhere.tsv" "$OLDM"; r=$(inst)
expect "refused (nothing vouches for the files)" [ "$r" = 1 ] && said "is a symbolic link; not imported" &&
expect "link kept" [ -L "$OLDM" ] && expect "target untouched" [ -s "$HOME/elsewhere.tsv" ] &&
expect "no new record" [ ! -e "$M" ] && ok || unnamed

case_ "4. old backups are listed and never deleted, even by --purge; names are printed escaped"
fresh; old_layout; mkdir -p "$OLDB"; echo old >"$OLDB/bitchatd.20250101-000000"; echo evil >"$OLDB/"$'evil\e[31mred'
r=$(inst); expect "install ok" [ "$r" = 0 ] && ok || unnamed
r=$(uninst --purge)
expect "exit 0" [ "$r" = 0 ] && expect "old backup kept" [ -f "$OLDB/bitchatd.20250101-000000" ] && [ -f "$OLDB/"$'evil\e[31mred' ] &&
expect "listed" said "kept in $OLDB" && said "$OLDB/bitchatd.20250101-000000" &&
expect "escaped" said "evil\\E[31mred" && expect "no raw escape in the output" not grep -q $'\e' "$HOME/out.txt" &&
expect "state folder kept for them" [ -d "$STATE" ] && ok || unnamed

case_ "5. the old empty lock goes with the old record; a non-empty one stays"
fresh; old_layout; : >"$STATE/.lock"; inst >/dev/null
expect "removed" [ ! -e "$STATE/.lock" ] && ok || unnamed
fresh; old_layout; echo x >"$STATE/.lock"; inst >/dev/null
expect "kept" [ -f "$STATE/.lock" ] && ok || unnamed

finish
