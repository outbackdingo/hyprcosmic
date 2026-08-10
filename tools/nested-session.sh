#!/usr/bin/bash
#
# Run the forked cosmic-session nested inside the live desktop, safely.
#
# WHY THIS SCRIPT EXISTS
# ----------------------
# On 2026-08-10 an ad-hoc version of this test logged the developer out of their
# own desktop. Two independent mistakes did it, and both are easy to repeat by
# hand, so the test lives in a script instead:
#
#   1. `pkill -x cosmic-session` matched the *real* session leader. The fork and
#      the system COSMIC ship binaries with the same name, so no name-based
#      match can distinguish them. This script therefore never uses pkill or
#      pgrep; it kills the process group it created, by ID.
#
#   2. A nested cosmic-session on the shared session bus takes the well-known
#      D-Bus name `com.system76.CosmicSession` away from the running session
#      (journal: "Connection `:1.3` lost name `com.system76.CosmicSession`").
#      That destabilises the outer desktop before anything is even killed. This
#      script always runs under `dbus-run-session`, so the nested session gets a
#      private bus and cannot touch the real one's names.
#
# Nesting cosmic-comp alone is safe and does not need any of this; the hazard is
# specific to running a second cosmic-session.
#
# Usage: tools/nested-session.sh [seconds] [-- extra env assignments]
#   e.g. tools/nested-session.sh 12 -- HYPRCOSMIC_PROFILE=hyprcosmic

set -uo pipefail

REPO="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
SESSION_BIN="$REPO/cosmic-session/target/debug/cosmic-session"
COMP_BIN="$REPO/cosmic-comp/target/debug/cosmic-comp"

DURATION="${1:-12}"
shift || true
[[ "${1:-}" == "--" ]] && shift

die() { printf 'nested-session: %s\n' "$*" >&2; exit 1; }

# Refuse to run outside a Wayland session. Without a host compositor the winit
# backend would fall back to DRM and try to take over the real display.
[[ -n "${WAYLAND_DISPLAY:-}" ]] || die "no WAYLAND_DISPLAY; refusing to run (would grab the DRM device)"
[[ -x "$SESSION_BIN" ]] || die "not built: $SESSION_BIN"
[[ -x "$COMP_BIN" ]]    || die "not built: $COMP_BIN"
command -v dbus-run-session >/dev/null || die "dbus-run-session is required for bus isolation"

# Record the live session's leader purely so the exit check can prove we did not
# disturb it. Asked of logind rather than matched by process name: a name-based
# lookup is what destroyed the developer's session twice, once in the very test
# written to prove name matching was unsafe. There is no `ps -C cosmic-...`
# anywhere in this file, deliberately.
OUTER_LEADER="$(loginctl show-session "${XDG_SESSION_ID:-}" -p Leader --value 2>/dev/null)"

LOG="$(mktemp -t nested-session.XXXXXX.log)"
echo "nested-session: logging to $LOG"
echo "nested-session: live session leader=$OUTER_LEADER (must survive)"

# setsid puts the whole tree in a fresh process group whose ID equals the child
# PID, so one negative kill reaps the session, the compositor and every
# component it spawned -- with no pattern matching anywhere.
setsid env \
    COSMIC_BACKEND=winit \
    RUST_LOG="${RUST_LOG:-info}" \
    "$@" \
    dbus-run-session -- "$SESSION_BIN" "$COMP_BIN" >"$LOG" 2>&1 &
PGID=$!

cleanup() {
    # Negative PID = process group. Never a name.
    kill -TERM -"$PGID" 2>/dev/null
    for _ in $(seq 20); do
        kill -0 -"$PGID" 2>/dev/null || break
        sleep 0.25
    done
    kill -KILL -"$PGID" 2>/dev/null

    # An abruptly-killed compositor leaves its IPC directory behind, so drop any
    # whose owning PID is gone. Matching is on the PID embedded in the name.
    for dir in "${XDG_RUNTIME_DIR:?}"/hypr/cosmic_*; do
        [[ -d "$dir" ]] || continue
        pid="${dir##*/cosmic_}"; pid="${pid%%_*}"
        kill -0 "$pid" 2>/dev/null || rm -rf "$dir"
    done
}
trap cleanup EXIT INT TERM

sleep "$DURATION"
cleanup
trap - EXIT INT TERM

# The whole point: confirm the developer still has a desktop.
status=0
if [[ -n "$OUTER_LEADER" ]] && ! kill -0 "$OUTER_LEADER" 2>/dev/null; then
    echo "nested-session: FAIL - live session leader $OUTER_LEADER died during the test" >&2
    status=1
else
    echo "nested-session: live session survived"
fi

echo "--- log: $LOG ---"
exit $status
