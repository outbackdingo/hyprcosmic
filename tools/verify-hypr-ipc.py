#!/usr/bin/env python3
"""Check that the fork's Hyprland IPC is reachable the way a real client reaches it.

Run this inside a HyprCosmic session after installing a new cosmic-comp. It
exercises the two things that are easy to get wrong and impossible to see from
the compositor's own log:

  1. The socket *names*. Every Hyprland client -- waybar's hyprland/* modules,
     hyprctl, eww, ags -- opens `$XDG_RUNTIME_DIR/hypr/$HIS/.socket.sock` and
     gives up if it is absent. waybar reports it once, at startup, as
     "Couldn't connect to ... (3)" and then disables the module, so a bar with
     a silently missing workspace widget is the only symptom you get.

  2. The dispatch (write) endpoint, which is what makes clicking a workspace on
     the bar actually switch to it rather than just look clickable.

Nothing here changes configuration. `dispatch workspace` moves the focused
workspace, which is runtime state, so this script does disturb what you are
looking at: it returns to the workspace you started on when it finishes.

Exit status is 0 only if every check passed.
"""

import os
import socket
import sys

RESET, RED, GREEN, DIM = "\033[0m", "\033[31m", "\033[32m", "\033[2m"

failures = []


def result(ok: bool, label: str, detail: str = "") -> bool:
    mark = f"{GREEN}ok{RESET}" if ok else f"{RED}FAIL{RESET}"
    print(f"  [{mark}] {label}")
    if detail:
        for line in str(detail).splitlines():
            print(f"         {DIM}{line}{RESET}")
    if not ok:
        failures.append(label)
    return ok


def socket_dir() -> str:
    runtime = os.environ.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}")
    base = os.path.join(runtime, "hypr")
    if not os.path.isdir(base):
        print(f"{RED}No {base}. Is this a HyprCosmic session?{RESET}")
        sys.exit(2)
    sig = os.environ.get("HYPRLAND_INSTANCE_SIGNATURE")
    if not sig:
        # Newest instance, so this still works from a terminal that predates it.
        entries = sorted(
            (e for e in os.listdir(base) if os.path.isdir(os.path.join(base, e))),
            key=lambda e: os.stat(os.path.join(base, e)).st_mtime,
        )
        if not entries:
            print(f"{RED}No instance directory under {base}.{RESET}")
            sys.exit(2)
        sig = entries[-1]
        print(f"{DIM}HYPRLAND_INSTANCE_SIGNATURE unset; using newest: {sig}{RESET}")
    return os.path.join(base, sig)


def request(path: str, payload: str, timeout: float = 2.0) -> str:
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
        s.settimeout(timeout)
        s.connect(path)
        s.sendall(payload.encode())
        chunks = []
        while True:
            data = s.recv(8192)
            if not data:
                break
            chunks.append(data)
    return b"".join(chunks).decode(errors="replace")


def main() -> int:
    d = socket_dir()
    req_path = os.path.join(d, ".socket.sock")
    evt_path = os.path.join(d, ".socket2.sock")

    print(f"\ninstance dir: {d}\n")

    print("socket names (the names clients actually open)")
    have_req = result(os.path.exists(req_path), ".socket.sock exists")
    result(os.path.exists(evt_path), ".socket2.sock exists")
    stale = [n for n in (".socket", ".socket2") if os.path.exists(os.path.join(d, n))]
    result(not stale, "no unsuffixed leftovers", ", ".join(stale) if stale else "")

    if not have_req:
        print(f"\n{RED}Request socket missing; cannot go further.{RESET}")
        print("An old cosmic-comp is probably still running -- the rename only")
        print("takes effect for a session started after installing the binary.")
        return 1

    print("\nread endpoints")
    active = None
    for cmd in ("workspaces", "activeworkspace", "activewindow", "clients", "monitors"):
        try:
            reply = request(req_path, f"j/{cmd}")
            ok = reply.strip().startswith(("{", "["))
            result(ok, f"j/{cmd}", "" if ok else f"unexpected reply: {reply[:200]}")
            if cmd == "activeworkspace" and ok:
                import json

                active = json.loads(reply).get("id")
        except OSError as e:
            result(False, f"j/{cmd}", e)

    print("\nunknown commands are refused, not guessed at")
    for cmd in ("bogus", "dispatch exec rofi", "dispatch killactive",
                "dispatch workspace +1", "dispatch workspace 0"):
        try:
            reply = request(req_path, cmd).strip()
            # An unparsed request gets no useful answer; what matters is that it
            # is not silently treated as something else.
            result(not reply.startswith("ok"), f"refuses {cmd!r}", f"reply: {reply[:120]}")
        except OSError as e:
            result(False, f"refuses {cmd!r}", e)

    print("\nwrite endpoint")
    if active is None:
        result(False, "know the current workspace to return to")
    else:
        target = 2 if active != 2 else 1
        try:
            reply = request(req_path, f"dispatch workspace {target}").strip()
            result(reply == "ok", f"dispatch workspace {target}", f"reply: {reply[:120]}")
            back = request(req_path, f"dispatch workspace {active}").strip()
            result(back == "ok", f"back to workspace {active}", f"reply: {back[:120]}")
        except OSError as e:
            result(False, "dispatch workspace", e)

    print()
    if failures:
        print(f"{RED}{len(failures)} check(s) failed:{RESET}")
        for f in failures:
            print(f"  - {f}")
        return 1
    print(f"{GREEN}All checks passed.{RESET}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
