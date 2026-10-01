#!/usr/bin/env bash
# Installer tests: each case runs dist/install.sh / dist/uninstall.sh
# against a fresh temporary HOME, with systemctl, omarchy, omarchy-shell and
# cargo replaced by stubs that only log what they were asked (cargo "builds"
# by copying placeholder binaries). Nothing outside the temporary HOMEs is
# touched; the real user's service is never queried.
#
#   bash tests/install/run.sh        every case
#   bash tests/install/run.sh 30     only cases/30-*.sh
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd -P)"
T="$ROOT/tests/install"
WORK="$(mktemp -d)"
trap 'chmod -R u+rwX -- "$WORK" 2>/dev/null; rm -rf -- "$WORK"' EXIT
export ROOT T WORK

# Placeholder binaries, different on every run.
export FAKE_BINS="$WORK/bins"
mkdir -p "$FAKE_BINS"
printf '#!/bin/sh\necho fake bitchatd %s\n' "$RANDOM$RANDOM" >"$FAKE_BINS/bitchatd"
printf '#!/bin/sh\necho fake bitchatctl %s\n' "$RANDOM$RANDOM" >"$FAKE_BINS/bitchatctl"
chmod +x "$FAKE_BINS"/*
export PATH="$T/stubs:$PATH"
unset XDG_CONFIG_HOME XDG_CACHE_HOME XDG_RUNTIME_DIR CARGO_TARGET_DIR

total=0 failed=0
for c in "$T"/cases/${1:-}*.sh; do
  [[ -f $c ]] || continue
  out="$(bash "$c" 2>&1)"; rc=$?
  printf '%s\n' "$out"
  n="$(grep -c '^   ok' <<<"$out")"; f="$(grep -c '^   FAIL' <<<"$out")"
  total=$((total + n + f)); failed=$((failed + f))
  (( rc == 0 )) || { echo "   FAIL: $(basename "$c") exited $rc"; failed=$((failed + 1)); }
done
echo
echo "$total checks, $failed failed"
(( failed == 0 ))
