#!/usr/bin/bash
#
# Install the parts of HyprCosmic that live outside a user's home directory.
#
# WHY THIS SCRIPT EXISTS
# ----------------------
# Everything here was placed by hand with `sudo install` while the desktop was
# being built, which left two problems that this script exists to close:
#
#   1. A fresh machine has none of it. rofi's config.rasi @imports
#      /usr/share/hyprcosmic/rofi/{palette,rules}.rasi by absolute path, and a
#      missing @import is an error rofi renders *in place of the launcher*
#      rather than a warning it skips. Miss those two files and Super+A shows a
#      parse error instead of a menu.
#
#   2. Hand-installed files drift. /usr/bin/start-hyprcosmic silently gained a
#      session-logging block during debugging that never made it back to the
#      copy under version control; nothing noticed until the two were diffed on
#      a hunch. `--check` is the cure: it compares every managed file against
#      its source and exits non-zero on any difference.
#
# WHAT IT DOES NOT DO
# -------------------
# Per-user files. `cosmic-conf import-theme --assets` writes those, because they
# depend on the installed theme and on $HOME; see PER_USER below for the list
# and its owner. The cosmic-conf, cosmic-session and cosmic-comp binaries are
# also out of scope -- they are build outputs, so `cargo` installs them and
# comparing them byte-for-byte would only ever report a rebuild.
#
# Usage:
#   tools/install-assets.sh              install (needs write access to PREFIX)
#   tools/install-assets.sh --check      compare only; exit 1 on any drift
#   tools/install-assets.sh --dry-run    print what install would do
#   tools/install-assets.sh --no-session skip start-hyprcosmic and the .desktop
#
# Environment:
#   PREFIX   install prefix, default /usr    (see the warning below)
#   DESTDIR  staging root for packaging, prepended to every path

set -uo pipefail

REPO="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
PREFIX="${PREFIX:-/usr}"
DESTDIR="${DESTDIR:-}"

MODE=install
WITH_SESSION=1

# Kept for the "re-run as sudo" hint, which has to quote the invocation the user
# actually made; $@ is long gone by the time a write fails.
ARGV=("$@")

die() { printf 'install-assets: %s\n' "$*" >&2; exit 1; }
warn() { printf 'install-assets: %s\n' "$*" >&2; }

while (($#)); do
    case "$1" in
        --check)      MODE=check ;;
        --dry-run)    MODE=dry-run ;;
        --no-session) WITH_SESSION=0 ;;
        -h|--help)    sed -n '2,/^set -uo/p' "${BASH_SOURCE[0]}" | sed 's/^# \?//;$d'; exit 0 ;;
        *)            die "unknown argument: $1 (try --help)" ;;
    esac
    shift
done

# Files under config/ that belong to the system, as "source:destination:mode".
# Destinations are relative to $PREFIX.
SHARED=(
    "config/waybar/bridge-hyde.css:share/hyprcosmic/waybar/bridge-hyde.css:644"
    "config/waybar/config.jsonc:share/hyprcosmic/waybar/config.jsonc:644"
    "config/waybar/palette.css:share/hyprcosmic/waybar/palette.css:644"
    "config/waybar/rules.css:share/hyprcosmic/waybar/rules.css:644"
    "config/rofi/palette.rasi:share/hyprcosmic/rofi/palette.rasi:644"
    "config/rofi/rules.rasi:share/hyprcosmic/rofi/rules.rasi:644"
)

# The session entry point. Kept apart from SHARED because it is versioned in the
# cosmic-session fork rather than in this repository -- that fork is a separate
# checkout and may simply be absent, in which case these are skipped.
SESSION=(
    "cosmic-session/data/start-hyprcosmic:bin/start-hyprcosmic:755"
    "cosmic-session/data/hyprcosmic.desktop:share/wayland-sessions/hyprcosmic.desktop:644"
)

# Files under config/ that produce a SHARED file rather than being one. They are
# inputs to a generator and have no place on the system: installing the template
# would put a file full of @@TOKEN@@ placeholders next to the real config, and
# whichever one a future reader opened first would be a coin toss.
SOURCES=(
    "config/waybar/config.jsonc.in"       # -> config/waybar/config.jsonc
    "config/waybar/generate-config.py"    # the generator, and the icon table
)

# Files under config/ that are deliberately NOT installed here, each with the
# thing that does install it. This list is not decoration: the audit below
# refuses to run unless every file under config/ appears in exactly one of the
# three lists, so adding a file forces a decision about where it belongs instead
# of letting it be quietly left out of all of them.
PER_USER=(
    "config/autostart"           # ~/.config/hyprcosmic/autostart, by hand
    "config/cosmic.conf"         # ~/.config/hyprcosmic/cosmic.conf, by hand
    "config/waybar/style.css"    # ~/.config/hyprcosmic/waybar/style.css; @imports a sibling theme.css
    "config/rofi/config.rasi"    # ~/.config/rofi/config.rasi, by `cosmic-conf import-theme --assets`
)

audit_config_tree() {
    local f rel known=" ${PER_USER[*]} ${SOURCES[*]} " unclassified=()
    # Built with a loop, not `${SHARED[*]%%:*}`: that form strips the suffix
    # from the first element only and silently keeps the rest whole.
    for f in "${SHARED[@]}"; do known+="${f%%:*} "; done

    while IFS= read -r -d '' f; do
        rel="${f#"$REPO"/}"
        [[ "$known" == *" $rel "* ]] || unclassified+=("$rel")
    done < <(find "$REPO/config" -type f -print0 | sort -z)

    ((${#unclassified[@]} == 0)) || die "not listed as shared or per-user: ${unclassified[*]}
  Add each to SHARED or PER_USER in $(basename "${BASH_SOURCE[0]}") and say which."
}

# The prefix is only half honoured, and pretending otherwise would be worse than
# saying so. Some consumers name /usr/share/hyprcosmic as a literal because they
# have no way to interpolate one: rofi's .rasi has no variables, and the
# autostart file is explicitly not a shell. Report them by grepping rather than
# from a hardcoded list, so this warning cannot go stale.
check_prefix_assumptions() {
    [[ "$PREFIX" == /usr ]] && return 0
    local hits
    hits="$(cd "$REPO" && grep -rl '/usr/share/hyprcosmic' config/ 2>/dev/null | sort | tr '\n' ' ')"
    [[ -z "$hits" ]] && return 0
    warn "PREFIX=$PREFIX, but these name /usr/share/hyprcosmic literally and cannot interpolate it:"
    warn "  $hits"
    warn "they will keep reading /usr/share unless you edit them; rofi will show a parse error if it is empty"
}

# Fail before touching anything rather than half way through. The nearest
# existing ancestor is what matters: install(1) creates the leaf directories, so
# an absent share/hyprcosmic/rofi is fine as long as share/ can be written.
assert_writable() {
    local dest="$1" dir
    dir="$(dirname "$dest")"
    while [[ ! -e "$dir" && "$dir" != / ]]; do dir="$(dirname "$dir")"; done
    [[ -w "$dir" ]] && return 0
    if ((EUID != 0)); then
        die "$dir is not writable. Re-run as: sudo ${BASH_SOURCE[0]}${ARGV[*]:+ ${ARGV[*]}}"
    fi
    die "$dir is not writable, even as root"
}

status=0
installed=0 skipped=0 differs=0

handle() {
    local src="$REPO/$1" dest="$DESTDIR$PREFIX/$2" mode="$3"

    if [[ ! -f "$src" ]]; then
        warn "missing source, skipped: $1"
        skipped=$((skipped + 1))
        return
    fi

    case "$MODE" in
        check)
            if [[ ! -e "$dest" ]]; then
                printf '  MISSING  %s\n' "$dest"
                differs=$((differs + 1))
            elif cmp -s "$src" "$dest"; then
                printf '  ok       %s\n' "$dest"
            else
                printf '  DIFFERS  %s\n' "$dest"
                differs=$((differs + 1))
            fi
            ;;
        dry-run)
            printf '  would install -m %s %s -> %s\n' "$mode" "$1" "$dest"
            ;;
        install)
            assert_writable "$dest"
            install -D -m "$mode" "$src" "$dest" || die "failed to install $dest"
            printf '  %s\n' "$dest"
            installed=$((installed + 1))
            ;;
    esac
}

audit_config_tree
[[ "$MODE" == install ]] && check_prefix_assumptions

targets=("${SHARED[@]}")
if ((WITH_SESSION)); then
    if [[ -d "$REPO/cosmic-session/data" ]]; then
        targets+=("${SESSION[@]}")
    else
        warn "cosmic-session/data is absent; skipping the session entry point"
    fi
fi

case "$MODE" in
    check)   echo "Checking against $DESTDIR$PREFIX:" ;;
    dry-run) echo "Dry run against $DESTDIR$PREFIX:" ;;
    install) echo "Installing to $DESTDIR$PREFIX:" ;;
esac

for spec in "${targets[@]}"; do
    IFS=: read -r src dest mode <<<"$spec"
    handle "$src" "$dest" "$mode"
done

case "$MODE" in
    check)
        if ((differs)); then
            echo "$differs file(s) missing or out of date; re-run without --check to fix" >&2
            status=1
        else
            echo "All ${#targets[@]} file(s) match."
        fi
        ;;
    install)
        echo "Installed $installed file(s)."
        ((skipped)) && echo "Skipped $skipped missing source(s)." >&2
        ;;
esac

exit $status
