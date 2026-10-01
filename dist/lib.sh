# Shared by install.sh and uninstall.sh. Sourced, not run.
#
# The one rule: bitchat replaces or removes a path only if it is a regular
# file (never a symlink, never a folder) whose SHA-256 matches either the
# line for that path in the install record, or a version of the unit file
# some commit of this project shipped (dist/known-hashes.tsv). A file that
# can't be read is never ours. Every replacement swaps the file out first
# and checks the bytes that came out, so an edit made after the check is
# kept as a backup, not lost. Everything else is yours: it stays, and the
# output says so.
#
# Record: ~/.local/state/omarchy-bitchat-install/installed.tsv, one
# "<sha256><TAB><absolute path>" per file bitchat wrote, plus
# "backup<TAB><original><TAB><backup>" for files moved aside with your
# consent (--replace-existing=<path>). Backups under
# ~/.local/state/omarchy-bitchat-install/backup/ are never deleted by these
# scripts. The record, lock and backups live outside the daemon's state
# folder on purpose: the unit lets the daemon write there, and it must never
# be able to vouch for a file.
#
# Imprecise on purpose: parent folders are resolved the way the OS does (a
# symlinked ~/.local/bin is followed; only the final path component is never
# a link); a unit you set back to the exact bytes of a shipped version counts
# as ours; without `mv --exchange` the swap is two renames with a tiny gap.

[[ -n ${BASH_VERSION:-} ]] || { echo "dist/lib.sh needs bash" >&2; return 1 2>/dev/null || exit 1; }

PLUGIN_ID="derekross.bitchat"
SERVICE="bitchat.service"
BINARIES=(bitchatd bitchatctl)

BINDIR="$HOME/.local/bin"
UNITDIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"   # systemd honours XDG_CONFIG_HOME
UNIT="$UNITDIR/$SERVICE"
PLUGINDIR="$HOME/.config/omarchy/plugins"                  # where Omarchy itself looks
PLUGIN_PATH="$PLUGINDIR/$PLUGIN_ID"
# Fixed (not XDG), matching Environment= and ReadWritePaths= in the unit.
DATADIR="$HOME/.local/share/omarchy-bitchat"
STATEDIR="$HOME/.local/state/omarchy-bitchat"
# The installer's own bookkeeping: a folder the daemon can't write.
INSTDIR="$HOME/.local/state/omarchy-bitchat-install"
RECORD="$INSTDIR/installed.tsv"
LOCKFILE="$INSTDIR/.lock"
BACKUPDIR="$INSTDIR/backup"
# Where versions before that kept it (inside the daemon's state folder).
OLD_RECORD="$STATEDIR/installed.tsv"
OLD_LOCKFILE="$STATEDIR/.lock"
OLD_BACKUPDIR="$STATEDIR/backup"
# The daemon's runtime folder (systemd RuntimeDirectory=bitchat), and the
# socket older builds made directly in $XDG_RUNTIME_DIR.
SOCKDIR="${XDG_RUNTIME_DIR:+$XDG_RUNTIME_DIR/bitchat}"
OLD_SOCKET="${XDG_RUNTIME_DIR:+$XDG_RUNTIME_DIR/bitchat.sock}"

say() { printf '%s\n' "$*"; }
# A name read from disk, safe to print: control characters escaped.
qp() {
  local LC_ALL=C
  if [[ $1 == *[$'\x01'-$'\x1f'$'\x7f']* || $1 == *$'\xc2'[$'\x80'-$'\x9f']* ]]; then
    printf '%q' "$1"
  else
    printf '%s' "$1"
  fi
}
note() { printf '  %s\n' "$*"; }
die() { printf '%s\n' "$*" >&2; exit 1; }

# ── Files ──────────────────────────────────────────────────────────────
path_kind() {
  if [[ -L $1 ]]; then echo symlink
  elif [[ ! -e $1 ]]; then echo missing
  elif [[ -f $1 ]]; then echo file
  elif [[ -d $1 ]]; then echo dir
  else echo other
  fi
}
# SHA-256 of a readable regular file. Prints nothing and fails otherwise:
# callers must treat an empty hash as "not ours".
file_hash() {
  local out
  [[ -f $1 && ! -L $1 ]] || return 1
  out="$(sha256sum -- "$1" 2>/dev/null)" || return 1
  out=${out%% *}
  [[ $out =~ ^[0-9a-f]{64}$ ]] || return 1
  printf '%s\n' "$out"
}
describe_file() {
  local size when
  size="$(stat -c %s -- "$1" 2>/dev/null || echo '?')"
  when="$(date -r "$1" '+%Y-%m-%d %H:%M' 2>/dev/null || echo '?')"
  printf '%s bytes, modified %s' "$size" "$when"
}

# Temp files this run made (all via mktemp, next to their destination);
# removed on exit. Never anything of yours.
TEMPS=()
cleanup_temps() { local t; for t in "${TEMPS[@]}"; do [[ -f $t && ! -L $t ]] && rm -f -- "$t"; done; return 0; }
# untemp <path>: it now holds your bytes, so the exit trap must leave it.
untemp() { local t keep=(); for t in "${TEMPS[@]}"; do [[ $t == "$1" ]] || keep+=("$t"); done; TEMPS=("${keep[@]}"); }
trap cleanup_temps EXIT

MV_EXCHANGE=0
mv --help 2>/dev/null | grep -q -- '--exchange' && MV_EXCHANGE=1

# ── State folder, lock, record ─────────────────────────────────────────
# A real folder of ours, never a link; refuse otherwise.
check_dir() {
  case "$(path_kind "$1")" in
    missing | dir) [[ ! -e $1 || -O $1 ]] || die "$1 belongs to another user; refusing to use it." ;;
    symlink) die "$1 is a symbolic link; bitchat doesn't follow links for its state. Move it aside, then run this again." ;;
    *) die "$1 exists and isn't a folder. Move it aside, then run this again." ;;
  esac
}
check_state_file() {
  case "$(path_kind "$1")" in
    missing | file) ;;
    symlink) die "$1 is a symbolic link; bitchat doesn't follow links for its state. Move it aside, then run this again." ;;
    *) die "$1 exists and isn't a regular file. Move it aside, then run this again." ;;
  esac
}

# Checks every state path, creates the installer's folder (0700) and takes
# the lock, so two runs (two checkouts, say) can't interleave.
prepare_state() {
  local tmp
  check_dir "$INSTDIR"; check_dir "$STATEDIR"; check_dir "$DATADIR"; check_dir "$BACKUPDIR"
  check_state_file "$RECORD"; check_state_file "$LOCKFILE"
  if [[ ! -d $INSTDIR ]]; then mkdir -p -m 700 -- "$INSTDIR" || die "can't create $INSTDIR"; fi
  check_dir "$INSTDIR"
  [[ -w $INSTDIR ]] || die "$INSTDIR isn't writable."
  if [[ ! -e $LOCKFILE && ! -L $LOCKFILE ]]; then
    # Created by link(2), which never follows or replaces anything.
    tmp="$(mktemp -- "$INSTDIR/.lock.XXXXXX")" || die "can't create a lock in $INSTDIR"
    ln -T -- "$tmp" "$LOCKFILE" 2>/dev/null || true
    rm -f -- "$tmp"
  fi
  check_state_file "$LOCKFILE"
  # Opened read-only: the lock never writes through whatever is at that path.
  exec 9<"$LOCKFILE" || die "can't open $LOCKFILE"
  flock -n 9 || die "another bitchat install or uninstall is running (lock: $LOCKFILE)."
}

declare -A RECORD_HASH=()   # absolute path -> sha256
BACKUP_LINES=()             # "original<TAB>backup"
# Only paths these scripts write are taken from the record, so a record
# that was edited can't point them at anything else.
recordable() {
  local b
  [[ $1 == "$UNIT" ]] && return 0
  for b in "${BINARIES[@]}"; do [[ $1 == "$BINDIR/$b" ]] && return 0; done
  return 1
}
# read_record <file>: its lines into RECORD_HASH / BACKUP_LINES.
read_record() {
  local file=$1 h p extra
  while IFS=$'\t' read -r h p extra || [[ -n $h ]]; do
    if [[ $h == backup ]]; then
      if [[ $p == /* && $extra == /* ]]; then BACKUP_LINES+=("$p"$'\t'"$extra"); fi
    elif [[ $h =~ ^[0-9a-f]{64}$ && -z $extra ]] && recordable "$p"; then
      RECORD_HASH["$p"]=$h
    elif [[ -n $h$p ]]; then
      note "ignoring a line in $file that isn't a path this script writes: $(qp "${p:-${h:0:20}}")"
    fi
  done <"$file"
}
# load_record: the record, or (once) the one older versions kept in the
# daemon's state folder. An imported line still only counts while the file
# on disk has exactly that hash (owned_file), and only for the three paths
# these scripts write.
load_record() {
  RECORD_HASH=(); BACKUP_LINES=()
  if [[ -f $RECORD && ! -L $RECORD ]]; then
    read_record "$RECORD"
  elif [[ -L $OLD_RECORD ]]; then
    note "$OLD_RECORD is a symbolic link; not imported (left as it is)."
  elif [[ -f $OLD_RECORD ]]; then
    read_record "$OLD_RECORD"
    write_record
    rm -f -- "$OLD_RECORD"
    note "moved the install record from $OLD_RECORD to $RECORD"
    # The lock older versions made there: only an empty regular file.
    if [[ -f $OLD_LOCKFILE && ! -L $OLD_LOCKFILE && ! -s $OLD_LOCKFILE ]]; then rm -f -- "$OLD_LOCKFILE"; fi
  fi
  return 0
}
# write_record: dies (even inside `|| …`) if the record can't be written,
# so nothing goes on unrecorded.
write_record() {
  local tmp p b
  check_state_file "$RECORD"
  tmp="$(mktemp -- "$INSTDIR/.installed.XXXXXX")" || die "can't write the install record in $INSTDIR; stopped. Files written so far may be unrecorded."
  TEMPS+=("$tmp")
  {
    for p in "${!RECORD_HASH[@]}"; do printf '%s\t%s\n' "${RECORD_HASH[$p]}" "$p"; done | LC_ALL=C sort -t $'\t' -k2
    for b in "${BACKUP_LINES[@]}"; do printf 'backup\t%s\n' "$b"; done
  } >"$tmp" || die "can't write the install record ($tmp); stopped. $RECORD is unchanged."
  chmod 600 -- "$tmp" || die "can't set permissions on $tmp; stopped. $RECORD is unchanged."
  mv -T -- "$tmp" "$RECORD" || die "can't replace $RECORD; stopped. The previous record is unchanged."
}

# ── What bitchat versions wrote ────────────────────────────────────────
declare -A KNOWN=()   # "sha256<TAB>logical path" -> label
load_known() {
  local h l logical
  KNOWN=()
  [[ -f dist/known-hashes.tsv ]] || die "dist/known-hashes.tsv is missing; run this from an omarchy-bitchat checkout."
  while IFS=$'\t' read -r h l logical || [[ -n $h ]]; do
    [[ $h == \#* || -z $logical || ! $h =~ ^[0-9a-f]{64}$ ]] && continue
    KNOWN["$h"$'\t'"$logical"]=$l
  done <dist/known-hashes.tsv
  # This checkout's unit counts too.
  h="$(file_hash dist/bitchat.service)" && KNOWN["$h"$'\t'unit]=checkout
  return 0
}

# owned_file <absolute path> <logical path>: the only ownership test. On
# success the hash that was checked is left in OWN_HASH, so the swap that
# follows compares against exactly what was judged.
OWN_HASH=
owned_file() {
  local p=$1 logical=$2 h
  OWN_HASH=
  [[ -f $p && ! -L $p ]] || return 1
  h="$(file_hash "$p")" || return 1
  [[ -n $h ]] || return 1
  if [[ -n ${RECORD_HASH[$p]:-} && ${RECORD_HASH[$p]} == "$h" ]] || [[ -n ${KNOWN["$h"$'\t'"$logical"]:-} ]]; then
    OWN_HASH=$h
    return 0
  fi
  return 1
}

# classify <path> <logical>: sets FILE_STATE to missing | symlink | other |
# owned | foreign (and OWN_HASH when owned). Not in a subshell, on purpose.
FILE_STATE=missing
classify() {
  OWN_HASH=
  case "$(path_kind "$1")" in
    missing) FILE_STATE=missing ;;
    symlink) FILE_STATE=symlink ;;
    dir | other) FILE_STATE=other ;;
    file) if owned_file "$1" "$2"; then FILE_STATE=owned; else FILE_STATE=foreign; fi ;;
  esac
}

# ── Backups, replacing, removing ───────────────────────────────────────
# backup_file <path> <name> [<original path>]: moves it into the backup
# folder under a name nobody else has (mktemp), records it (as a backup of
# <original path>, default <path>), leaves the new path in BACKUP_DEST.
BACKUP_DEST=
backup_file() {
  local src=$1 name=$2 orig=${3:-$1} dest
  check_dir "$BACKUPDIR"
  [[ -d $BACKUPDIR ]] || mkdir -m 700 -- "$BACKUPDIR" || die "can't create $BACKUPDIR"
  check_dir "$BACKUPDIR"
  dest="$(mktemp -- "$BACKUPDIR/$name.$(date +%Y%m%dT%H%M%S).XXXXXX")" || die "can't create a backup in $BACKUPDIR"
  if ! mv -T -- "$src" "$dest"; then
    rm -f -- "$dest"
    die "couldn't move $src to $BACKUPDIR; stopped."
  fi
  BACKUP_LINES+=("$orig"$'\t'"$dest")
  write_record
  BACKUP_DEST=$dest
}

# replace_owned <source> <dest> <mode> <expected hash of what's at dest>
# With an empty expected hash nothing may be at dest (link(2) refuses
# otherwise). With one, the new file is swapped in and the file that came
# out is checked: if it isn't the expected bytes any more it is kept as a
# backup, and said so.
replace_owned() {
  local src=$1 dest=$2 mode=$3 expect=$4 dir tmp out newhash
  dir="$(dirname -- "$dest")"
  mkdir -p -- "$dir"
  tmp="$(mktemp -- "$dir/.bitchat.XXXXXX")" || { note "can't write in $dir; $dest not installed."; return 1; }
  TEMPS+=("$tmp")
  cp -- "$src" "$tmp" && chmod "$mode" -- "$tmp" || { note "couldn't stage $dest; not installed."; return 1; }
  newhash="$(file_hash "$tmp")" || { note "couldn't read back $tmp; $dest not installed."; return 1; }
  if [[ -z $expect ]]; then
    if ! ln -T -- "$tmp" "$dest" 2>/dev/null; then
      note "$dest appeared while installing; left as it is, not installed."
      return 1
    fi
    rm -f -- "$tmp"
  else
    if (( MV_EXCHANGE )) && mv --exchange -T -- "$tmp" "$dest" 2>/dev/null; then
      out=$tmp
      untemp "$out"
    else
      # No atomic exchange: two renames, a moment with nothing at dest.
      out="$(mktemp -- "$dir/.bitchat.XXXXXX")" || { note "can't write in $dir; $dest not replaced."; return 1; }
      TEMPS+=("$out")
      if ! mv -T -- "$dest" "$out" 2>/dev/null; then
        note "couldn't move $dest aside; not replaced."
        return 1
      fi
      untemp "$out"
      if ! mv -T -- "$tmp" "$dest"; then
        if mv -T -- "$out" "$dest" 2>/dev/null; then
          note "couldn't put the new $dest in place; the previous file is back where it was."
        else
          note "couldn't put the new $dest in place, nor move the previous file back: it is now $out"
        fi
        return 1
      fi
    fi
    if [[ -L $out || "$(file_hash "$out" || true)" != "$expect" ]]; then
      backup_file "$out" "$(basename -- "$dest")" "$dest"
      note "$dest changed after it was checked; what was there is kept in $BACKUP_DEST"
    else
      rm -f -- "$out"
    fi
  fi
  RECORD_HASH["$dest"]=$newhash
  write_record
}

# remove_owned <path> <expected hash>: moves it aside, checks it, removes it.
remove_owned() {
  local p=$1 expect=$2 out
  out="$(mktemp -- "$(dirname -- "$p")/.bitchat.XXXXXX")" || { note "can't write next to $p; left as it is."; return 1; }
  TEMPS+=("$out")
  if ! mv -T -- "$p" "$out" 2>/dev/null; then note "couldn't move $p aside; left as it is."; return 1; fi
  untemp "$out"
  if [[ -L $out || "$(file_hash "$out" || true)" != "$expect" ]]; then
    backup_file "$out" "$(basename -- "$p")" "$p"
    note "$p changed after it was checked; kept in $BACKUP_DEST"
  else
    rm -f -- "$out"
    note "removed $p"
  fi
  unset 'RECORD_HASH[$p]'
  write_record
}

# ── systemd ────────────────────────────────────────────────────────────
systemctl_user() {
  local out
  if out="$(systemctl --user "$@" 2>&1)"; then return 0; fi
  note "systemctl --user $*: ${out:-failed}"
  return 1
}

# How systemd sees the unit, and what is at our path.
UNIT_KIND=missing UNIT_STATE=missing UNIT_FRAGMENT='' UNIT_EXECSTART='' UNIT_LOAD='' UNIT_DROPIN=0
inspect_unit() {
  local show
  UNIT_KIND="$(path_kind "$UNIT")"
  UNIT_DROPIN=0; [[ -d $UNIT.d ]] && UNIT_DROPIN=1
  show="$(systemctl --user show -p FragmentPath -p ExecStart -p LoadState "$SERVICE" 2>/dev/null || true)"
  UNIT_FRAGMENT="$(printf '%s\n' "$show" | sed -n 's/^FragmentPath=//p' | head -1)"
  UNIT_EXECSTART="$(printf '%s\n' "$show" | sed -n 's/^ExecStart=//p')"
  UNIT_LOAD="$(printf '%s\n' "$show" | sed -n 's/^LoadState=//p' | head -1)"
  if [[ $UNIT_LOAD == masked || $UNIT_FRAGMENT == /dev/null ]]; then UNIT_STATE=masked
  elif [[ $UNIT_KIND == symlink ]]; then UNIT_STATE=symlink
  elif [[ -n $UNIT_FRAGMENT && $UNIT_FRAGMENT != "$UNIT" ]]; then UNIT_STATE=elsewhere
  else
    case $UNIT_KIND in
      missing) UNIT_STATE=missing ;;
      dir | other) UNIT_STATE=other ;;
      file)
        if owned_file "$UNIT" unit; then UNIT_STATE=owned
        # Only for the message: an edited unit is kept, never replaced.
        elif grep -qE -- 'omarchy-bitchat|/bitchatd' "$UNIT" 2>/dev/null; then UNIT_STATE=edited
        else UNIT_STATE=foreign; fi ;;
    esac
  fi
}
unit_runs_our_binary() { [[ $UNIT_EXECSTART == *"path=$BINDIR/bitchatd ;"* ]]; }
unit_is_active() { systemctl --user is-active --quiet "$SERVICE" 2>/dev/null; }
# The explanation for a unit state we never touch.
unit_refusal() {
  case $UNIT_STATE in
    masked) echo "$SERVICE is masked (systemctl --user mask). bitchat won't touch it; unmask it with: systemctl --user unmask $SERVICE" ;;
    symlink) echo "$UNIT is a symbolic link (to $(readlink -- "$UNIT")), so it was linked or masked by hand. bitchat won't follow or replace it." ;;
    elsewhere) echo "$SERVICE is loaded from $UNIT_FRAGMENT, not $UNIT. bitchat won't shadow or touch it; remove or rename that unit first." ;;
    other) echo "$UNIT exists and isn't a regular file. Move it aside, then run this again." ;;
  esac
}

# ── The plugin link ────────────────────────────────────────────────────
# this_checkout_is_plugin_dir: installed with `omarchy plugin add`, this
# checkout is the plugin folder itself (not a link to it).
this_checkout_is_plugin_dir() {
  [[ -d $PLUGIN_PATH && ! -L $PLUGIN_PATH && "$(realpath -e -- "$PLUGIN_PATH" 2>/dev/null)" == "$REPO" ]]
}
plugin_link_is_ours() {
  [[ -L $PLUGIN_PATH && "$(realpath -e -- "$PLUGIN_PATH" 2>/dev/null)" == "$REPO" ]]
}

# Both backup folders: this one, and the one older versions used in the
# daemon's state folder (left where it is).
list_backups() {
  local dir f found
  for dir in "$BACKUPDIR" "$OLD_BACKUPDIR"; do
    [[ -d $dir && ! -L $dir ]] || continue
    found=0
    for f in "$dir"/* "$dir"/.[!.]*; do
      [[ -e $f || -L $f ]] || continue
      (( found )) || say "Files bitchat moved aside are kept in $dir (never deleted by these scripts):"
      found=1
      note "$(qp "$f")"
    done
  done
  return 0
}
