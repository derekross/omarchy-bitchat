# Sourced by every case. A case is a sequence of `case_ "…"` headings, each
# followed by `expect` checks and a final `ok`.
set -u
: "${ROOT:?}" "${T:?}" "${WORK:?}" "${FAKE_BINS:?}"
FAILS=0
REAL_HOME=$HOME

# A new, empty HOME inside the test's temp dir. Stub knobs reset to
# "service not running, systemd sees whatever is at our unit path".
fresh() {
  export HOME
  HOME="$(mktemp -d -- "$WORK/home.XXXXXX")"
  [[ $HOME == "$WORK"/* && $HOME != "$REAL_HOME" ]] || { echo "bad test HOME" >&2; exit 2; }
  : >"$HOME/.bitchat-test-home"
  unset XDG_CONFIG_HOME XDG_CACHE_HOME FAKE_FRAGMENT FAKE_EXECSTART FAKE_LOAD
  export XDG_RUNTIME_DIR="$HOME/run"; mkdir -m 700 "$XDG_RUNTIME_DIR"
  export FAKE_LOG="$HOME/calls.log" FAKE_ACTIVE_RC=3
  export CARGO_TARGET_DIR="$HOME/target"
  : >"$FAKE_LOG"
  BIN="$HOME/.local/bin"
  U="$HOME/.config/systemd/user/bitchat.service"
  P="$HOME/.config/omarchy/plugins/derekross.bitchat"
  DATA="$HOME/.local/share/omarchy-bitchat"
  STATE="$HOME/.local/state/omarchy-bitchat"
  INST="$HOME/.local/state/omarchy-bitchat-install"
  M="$INST/installed.tsv"
  B="$INST/backup"
  OLDM="$STATE/installed.tsv"
  OLDB="$STATE/backup"
  OURS="{ path=$BIN/bitchatd ; argv[]=$BIN/bitchatd ; ignore_errors=no }"
  mkdir -p "$BIN"
}
inst() { (cd "$ROOT" && ./dist/install.sh "$@" </dev/null >"$HOME/out.txt" 2>&1); echo $?; }
uninst() { (cd "$ROOT" && ./dist/uninstall.sh "$@" </dev/null >"$HOME/out.txt" 2>&1); echo $?; }
out() { cat "$HOME/out.txt"; }
said() { grep -qF -- "$1" "$HOME/out.txt"; }
logged() { grep -qF -- "$1" "$FAKE_LOG"; }
not_logged() { ! grep -qF -- "$1" "$FAKE_LOG"; }
count_logged() { grep -cF -- "$1" "$FAKE_LOG"; }
record_lists() { [[ -f $M ]] && awk -F'\t' -v p="$1" '$2 == p { found = 1 } END { exit !found }' "$M"; }
record_verifies() { [[ -f $M ]] && awk -F'\t' '$1 != "backup" { print $1 "  " $2 }' "$M" | (cd / && sha256sum -c --quiet --strict >/dev/null 2>&1); }
same() { cmp -s -- "$1" "$2"; }
mode() { stat -c %a -- "$1"; }
no_temps() { [[ -z "$(find "$HOME" \( -name '.bitchat.*' -o -name '.installed.*' -o -name '.lock.*' \) -print -quit)" ]]; }
backups() { ls -- "$B" 2>/dev/null | grep -c "^$1\." || true; }
make_socket() {
  command -v python3 >/dev/null || return 1
  python3 -c 'import socket,sys; s=socket.socket(socket.AF_UNIX); s.bind(sys.argv[1])' "$1"
}

case_() { echo "$1"; }
JUST_FAILED=0
ok() { JUST_FAILED=0; echo "   ok"; }
# Ends every `… && ok` chain: a bare check that failed without `expect`
# still counts as a failure.
unnamed() { if (( JUST_FAILED )); then JUST_FAILED=0; else fail "a check in this step (no name)"; fi; }
fail() { echo "   FAIL: $*"; sed 's/^/     | /' "$HOME/out.txt" | head -40; echo "     | calls: $(tr '\n' ';' <"$FAKE_LOG" | cut -c1-400)"; FAILS=$((FAILS + 1)); }
# expect "<what>" <command...>: runs the command; a failure names it.
expect() { local what=$1; shift; if "$@"; then return 0; fi; fail "$what"; JUST_FAILED=1; return 1; }
not() { ! "$@"; }
finish() { exit $(( FAILS > 0 )); }
