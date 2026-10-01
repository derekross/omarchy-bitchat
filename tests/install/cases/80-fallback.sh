#!/usr/bin/env bash
source "$T/lib.sh"

# A mv that fails when moving onto $FAIL_MV_TARGET, and has no --exchange.
mkdir -p "$WORK/failmv"; cat >"$WORK/failmv/mv" <<'SH'
#!/usr/bin/env bash
if [[ ${1:-} == --help ]]; then PATH="${PATH#*failmv:}" mv --help | grep -v exchange; exit 0; fi
for a in "$@"; do [[ $a == --exchange ]] && exit 1; done
[[ -n ${FAIL_MV_TARGET:-} && ${!#} == "$FAIL_MV_TARGET" ]] && { echo "mv: simulated failure" >&2; exit 1; }
PATH="${PATH#*failmv:}" exec mv "$@"
SH
chmod +x "$WORK/failmv/mv"

case_ "1. two-rename swap whose second rename and restore both fail: your file is kept and its path given"
fresh; inst >/dev/null; h="$(sha256sum <"$BIN/bitchatd" | cut -d' ' -f1)"; cp "$BIN/bitchatd" "$HOME/before"
(cd "$ROOT" && REPO=$ROOT && source dist/lib.sh && prepare_state && load_record && MV_EXCHANGE=0 &&
  PATH="$WORK/failmv:$PATH" FAIL_MV_TARGET="$BIN/bitchatd" replace_owned "$FAKE_BINS/bitchatctl" "$BIN/bitchatd" 755 "$h" >"$HOME/out.txt" 2>&1); r=$?
kept="$(sed -n 's/.*nor move the previous file back: it is now //p' "$HOME/out.txt")"
expect "failed" [ "$r" != 0 ] && expect "said where" [ -n "$kept" ] &&
expect "the file is still there after exit" [ -f "$kept" ] && same "$HOME/before" "$kept" &&
expect "doesn't claim it's back" not said "is back where it was" && ok || unnamed

case_ "2. second rename fails, restore works: said so, file back in place"
fresh; inst >/dev/null; h="$(sha256sum <"$BIN/bitchatd" | cut -d' ' -f1)"; cp "$BIN/bitchatd" "$HOME/before"
cat >"$WORK/failmv/mv" <<'SH'
#!/usr/bin/env bash
if [[ ${1:-} == --help ]]; then PATH="${PATH#*failmv:}" mv --help | grep -v exchange; exit 0; fi
for a in "$@"; do [[ $a == --exchange ]] && exit 1; done
# Fail only the move of the staged new file (.bitchat.* that holds new bytes) onto the target.
if [[ -n ${FAIL_MV_TARGET:-} && ${!#} == "$FAIL_MV_TARGET" ]] && cmp -s -- "${@: -2:1}" "$FAKE_BINS/bitchatctl"; then echo "mv: simulated failure" >&2; exit 1; fi
PATH="${PATH#*failmv:}" exec mv "$@"
SH
(cd "$ROOT" && REPO=$ROOT && source dist/lib.sh && prepare_state && load_record && MV_EXCHANGE=0 &&
  PATH="$WORK/failmv:$PATH" FAIL_MV_TARGET="$BIN/bitchatd" replace_owned "$FAKE_BINS/bitchatctl" "$BIN/bitchatd" 755 "$h" >"$HOME/out.txt" 2>&1); r=$?
expect "failed" [ "$r" != 0 ] && expect "back" same "$HOME/before" "$BIN/bitchatd" && said "is back where it was" &&
expect "no temp left" no_temps && ok || unnamed

finish
