#!/usr/bin/env bash
source "$T/lib.sh"

case_ "1. a unit you edited is kept and named; the binaries still update; restarted if it runs ours"
fresh; inst >/dev/null
echo "Environment=RUST_LOG=debug" >>"$U"; cp "$U" "$HOME/mine"; : >"$FAKE_LOG"
printf '#!/bin/sh\necho newer\n' >"$HOME/target/release/bitchatd"
r=$(FAKE_ACTIVE_RC=0 inst --no-build)
expect "exit 0" [ "$r" = 0 ] && expect "kept" same "$HOME/mine" "$U" && expect "said" said "keeping your $U: you changed it" &&
expect "binary updated" grep -q newer "$BIN/bitchatd" &&
expect "not in the record" not record_lists "$U" &&
expect "restarted (it runs our binary)" logged "restart bitchat.service" && expect "no enable" not_logged "enable" && ok || unnamed

case_ "2. uninstall keeps the edited unit, doesn't stop or disable it, says how"
: >"$FAKE_LOG"; r=$(uninst)
expect "exit 0" [ "$r" = 0 ] && expect "kept" same "$HOME/mine" "$U" &&
expect "not stopped" not_logged "--user stop" && expect "not disabled" not_logged "--user disable" &&
expect "hint" said "systemctl --user disable --now bitchat.service && rm $U" && ok || unnamed

case_ "3. --replace-existing=<unit> puts bitchat's back, keeping yours as a backup"
fresh; inst >/dev/null; echo "# mine" >>"$U"; cp "$U" "$HOME/mine"
r=$(inst --no-build "--replace-existing=$U")
expect "exit 0" [ "$r" = 0 ] && expect "ours" same "$ROOT/dist/bitchat.service" "$U" && expect "backup" same "$HOME/mine" "$B"/bitchat.service.* && ok || unnamed

case_ "4. a unit that isn't bitchat's at our path stops the install; uninstall leaves it"
fresh; mkdir -p "$(dirname "$U")"; printf '[Service]\nExecStart=/usr/bin/true\n' >"$U"; r=$(inst)
expect "stop" [ "$r" = 1 ] && said "$U exists but bitchat has no record" && expect "no binary" [ ! -e "$BIN/bitchatd" ] && not_logged cargo && ok || unnamed
r=$(uninst); expect "left" [ -f "$U" ] && said "keeping $U" && expect "not disabled" not_logged "--user disable" && ok || unnamed

case_ "5. a masked unit (link to /dev/null): refused, untouched by install and uninstall"
fresh; mkdir -p "$(dirname "$U")"; ln -s /dev/null "$U"; r=$(inst)
expect "stop" [ "$r" = 1 ] && said "masked" && expect "still the mask" [ "$(readlink "$U")" = /dev/null ] && not_logged "enable" && ok || unnamed
r=$(inst "--replace-existing=$U"); expect "consent doesn't override a mask" [ "$r" = 1 ] && [ -L "$U" ] && ok || unnamed
r=$(uninst); expect "left" [ -L "$U" ] && said "masked" && expect "not disabled" not_logged "--user disable" && not_logged "--user stop" && ok || unnamed

case_ "6. masked elsewhere (LoadState=masked) is refused too"
fresh; r=$(FAKE_LOAD=masked FAKE_FRAGMENT=/dev/null inst)
expect "stop" [ "$r" = 1 ] && said "masked" && expect "nothing written" [ ! -e "$U" ] && [ ! -e "$BIN/bitchatd" ] && ok || unnamed

case_ "7. a unit linked by hand (symlink to a copy of ours): refused, not followed"
fresh; mkdir -p "$(dirname "$U")"; cp "$ROOT/dist/bitchat.service" "$HOME/linked.service"; ln -s "$HOME/linked.service" "$U"; r=$(inst)
expect "stop" [ "$r" = 1 ] && said "is a symbolic link" && [ -L "$U" ] && ok || unnamed
r=$(uninst); expect "uninstall leaves it" [ -L "$U" ] && [ -f "$HOME/linked.service" ] && ok || unnamed

case_ "8. a unit loaded from elsewhere on systemd's path: install refused; uninstall leaves it"
fresh; r=$(FAKE_FRAGMENT="$HOME/.local/share/systemd/user/bitchat.service" inst)
expect "stop" [ "$r" = 1 ] && said "is loaded from $HOME/.local/share/systemd/user/bitchat.service" && expect "nothing written" [ ! -e "$U" ] && ok || unnamed
fresh; inst >/dev/null; : >"$FAKE_LOG"
r=$(FAKE_FRAGMENT=/etc/systemd/user/bitchat.service FAKE_EXECSTART="$OURS" uninst)
expect "exit 0" [ "$r" = 0 ] && said "is loaded from /etc/systemd/user/bitchat.service" &&
expect "not stopped" not_logged "--user stop" && expect "not disabled" not_logged "--user disable" && expect "our file there kept" [ -f "$U" ] && ok || unnamed

case_ "9. drop-ins are mentioned, never touched"
fresh; mkdir -p "$U.d"; echo "[Service]" >"$U.d/override.conf"; r=$(inst)
expect "exit 0" [ "$r" = 0 ] && said "drop-ins are yours" && [ -f "$U.d/override.conf" ] && ok || unnamed
r=$(uninst); expect "drop-in kept" [ -f "$U.d/override.conf" ] && ok || unnamed

finish
