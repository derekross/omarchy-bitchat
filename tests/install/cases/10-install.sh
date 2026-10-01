#!/usr/bin/env bash
source "$T/lib.sh"

case_ "1. fresh install: builds, writes everything, records it, links the plugin, enables the service once"
fresh; r=$(inst)
expect "exit 0" [ "$r" = 0 ] &&
expect "built with cargo" logged "cargo build --release --locked --quiet -p bitchatd -p bitchatctl CARGO_TARGET_DIR=$HOME/target" &&
expect "bitchatd" same "$FAKE_BINS/bitchatd" "$BIN/bitchatd" &&
expect "bitchatctl" same "$FAKE_BINS/bitchatctl" "$BIN/bitchatctl" &&
expect "binary mode" [ "$(mode "$BIN/bitchatd")" = 755 ] &&
expect "unit" same "$ROOT/dist/bitchat.service" "$U" &&
expect "unit mode" [ "$(mode "$U")" = 644 ] &&
expect "plugin link" [ -L "$P" ] && [ "$(readlink "$P")" = "$ROOT" ] &&
expect "data dir 0700" [ "$(mode "$DATA")" = 700 ] &&
expect "state dir 0700" [ "$(mode "$STATE")" = 700 ] &&
expect "record verifies" record_verifies &&
expect "record lists all three" record_lists "$BIN/bitchatd" && record_lists "$BIN/bitchatctl" && record_lists "$U" &&
expect "record mode" [ "$(mode "$M")" = 600 ] &&
expect "enable --now once" [ "$(count_logged 'enable --now bitchat.service')" = 1 ] &&
expect "no restart" not_logged "restart" &&
expect "rescanned" logged "omarchy-shell shell rescanPlugins" &&
expect "enable hint" said "omarchy plugin enable derekross.bitchat --section right" &&
expect "status hint" said "bitchatctl status" && said "journalctl --user -u bitchat.service" &&
expect "no temp files" no_temps && ok || unnamed

case_ "2. reinstall over its own files: replaced quietly, service not enabled again, not started when stopped"
printf '#!/bin/sh\necho newer\n' >"$HOME/target/release/bitchatd"
: >"$FAKE_LOG"; r=$(inst --no-build)
expect "exit 0" [ "$r" = 0 ] &&
expect "replaced" grep -q newer "$BIN/bitchatd" &&
expect "nothing kept" not said "keeping" &&
expect "no backups" [ ! -e "$B" ] &&
expect "no build" not_logged "cargo" &&
expect "no enable" not_logged "enable" &&
expect "not restarted" not_logged "restart" &&
expect "told" said "isn't running; not started" &&
expect "record verifies" record_verifies && record_lists "$BIN/bitchatd" &&
expect "link kept" [ "$(readlink "$P")" = "$ROOT" ] &&
expect "no temp files" no_temps && ok || unnamed

case_ "3. reinstall while running our binary: restarted; running something else: told, not restarted"
: >"$FAKE_LOG"; r=$(FAKE_ACTIVE_RC=0 inst --no-build)
expect "exit 0" [ "$r" = 0 ] && expect "restart" logged "restart bitchat.service" && expect "no enable" not_logged "enable" && ok || unnamed
: >"$FAKE_LOG"; r=$(FAKE_ACTIVE_RC=0 FAKE_EXECSTART="{ path=/opt/bitchatd ; argv[]=/opt/bitchatd }" inst --no-build)
expect "not restarted" not_logged "restart" && expect "told" said "restart it yourself" && ok || unnamed

case_ "4. --no-start installs without enabling, starting or restarting"
fresh; r=$(inst --no-start)
expect "exit 0" [ "$r" = 0 ] && expect "installed" [ -f "$BIN/bitchatd" ] &&
expect "no enable" not_logged "enable" && expect "said" said "Not starting bitchat.service (--no-start)" && ok || unnamed
: >"$FAKE_LOG"; r=$(FAKE_ACTIVE_RC=0 inst --no-build --no-start)
expect "no restart" not_logged "restart" && ok || unnamed

case_ "5. the install's bookkeeping lives outside the daemon's writable folder"
fresh; r=$(inst)
expect "exit 0" [ "$r" = 0 ] && expect "record in the install folder" [ -f "$M" ] &&
expect "install folder 0700" [ "$(mode "$INST")" = 700 ] && expect "lock there" [ -f "$INST/.lock" ] &&
expect "nothing of the installer's in the daemon's folder" [ -z "$(ls -A "$STATE")" ] && ok || unnamed

case_ "6. a unit some old version shipped counts as ours (known-hashes.tsv), with no record"
fresh; mkdir -p "$(dirname "$U")"; printf '[Unit]\nDescription=old bitchat\n' >"$U"
(cd "$ROOT" && cp dist/known-hashes.tsv "$HOME/known.bak")
printf '%s\tv0\tunit\n' "$(sha256sum <"$U" | cut -d' ' -f1)" >>"$ROOT/dist/known-hashes.tsv"
r=$(inst); cp "$HOME/known.bak" "$ROOT/dist/known-hashes.tsv"
expect "exit 0" [ "$r" = 0 ] && expect "replaced" same "$ROOT/dist/bitchat.service" "$U" && expect "no backup" [ ! -e "$B" ] && ok || unnamed

case_ "7. a plugin path that isn't a link to this checkout is left alone"
fresh; mkdir -p "$(dirname "$P")"; ln -s /nonexistent "$P"; r=$(inst)
expect "exit 0" [ "$r" = 0 ] && expect "untouched" [ "$(readlink "$P")" = /nonexistent ] && said "Keeping $P" && ok || unnamed
fresh; mkdir -p "$P"; echo x >"$P/mine"; r=$(inst)
expect "folder untouched" [ -f "$P/mine" ] && [ ! -L "$P" ] && said "Keeping $P" && ok || unnamed

case_ "8. options"
fresh
expect "unknown option" [ "$(inst --bogus)" = 2 ] &&
expect "bare --replace-existing refused" [ "$(inst --replace-existing)" = 1 ] && said "needs the absolute path" &&
expect "a path we don't write refused" [ "$(inst --replace-existing=$HOME/.bashrc)" = 1 ] && said "not a path this script writes" &&
expect "nothing written" [ ! -e "$BIN/bitchatd" ] &&
expect "uninstall unknown option" [ "$(uninst --purg)" = 2 ] &&
expect "help" [ "$(inst --help)" = 0 ] && said "--replace-existing=<path>" && ok || unnamed

finish
