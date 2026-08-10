# A session that came up with no input, once, on 2026-08-10

**Status: not root-caused. Not reproduced since. Closed deliberately, not fixed.**

This is written down because the next person to hit it -- probably us -- will
otherwise start the same investigation from scratch, and because most of the
value here is the list of things it is *not*.

## What happened

One HyprCosmic session came up with a working bar and a blank desktop below it,
and no keyboard or pointer input reached the compositor at all. No binding
fired. The session had to be left via a VT switch.

After a reboot, the same configuration and the same binaries came up fine.
Super+Return, Super+A and a bare Super tap were all confirmed working by the
user, with independent evidence from a watcher process:

```
12:07:48 SPAWN pid=6264 exe=/usr/bin/rofi cmd=rofi -show drun
12:07:50 SPAWN pid=6433 exe=/usr/bin/rofi cmd=rofi -show drun
12:07:51 EVENT activewindow>>com.system76.CosmicTerm,dingo@fedora:~ - COSMIC Terminal
```

Super+Return shows as a window event rather than a spawn because cosmic-term is
single-instance: the binding fired, the existing process took the request.

## Ruled out, with the evidence

Each of these was a live hypothesis that turned out to be wrong. They are listed
so nobody re-runs them.

- **The fork's own patches.** The blank screen was a separate bug entirely (a
  missing `awww img` call in autostart -- the daemon was running and drawing
  nothing, so nothing reported a wallpaper missing). Patch B's IPC answered
  queries correctly throughout. Neither patch touches input.

- **Events dropped by `seats.for_device()` returning `None`.** This does drop
  input silently (`src/input/mod.rs:208-215`), which made it an attractive
  theory. But input demonstrably works on the same build, so whatever the fault
  was, it was not a permanent property of this code.

- **The modifier-only Super binding swallowing Super+Return.** It cannot. The
  match loop at `src/input/mod.rs:1915-1955` sets `modifiers_shortcut_queue` on
  press and fires on release, and critically it *does not early-return*, so a
  modifier-only binding cannot consume a normal binding sharing its modifier.

- **A shortcuts-config race at login.** The config's mtime was 08:49; the
  session started at 09:59:46. Nothing was being written during startup.

- **`Error reading from session socket` and `Unable to become drm master`.**
  Both appear in the log. Both also appear in stock COSMIC logins on this
  machine, so neither is a fork symptom. (An earlier claim in this project that
  the DRM message was absent post-reboot was wrong; it is present twice, for
  PID 1609.)

## The one thing that is suspicious

The broken session was the **fourth** compositor start on that boot: 08:06 (from
`target/debug`), 08:08, 08:19, and 09:59. Every other session on that boot, and
every session since a reboot, has been fine.

That points at accumulated per-boot session/seat state -- a previous compositor
not having fully released its seat, or logind still holding devices for a
session that had gone away -- rather than at anything in the configuration or
the code. It is a guess. It was not confirmed, and confirming it would mean
deliberately cycling compositors on a live desktop.

## If it happens again

Collect *before* rebooting, because a reboot destroys the only evidence:

1. `loginctl list-sessions` and `loginctl session-status` for each -- look for
   more than one active session, or a session in state `closing`.
2. `ls -l /dev/input/by-path/` and whether the compositor's PID holds any of
   them open (`ls -l /proc/<pid>/fd | grep event`).
3. The session log with `RUST_LOG=cosmic_comp::input=trace`. The logger honours
   `RUST_LOG` via `EnvFilter::try_from_default_env()` before adding its own
   `cosmic_comp={warn|debug}` directives, and those are less specific, so the
   trace directive wins.
4. Whether `libinput debug-events` (as root, on a VT) sees the devices at all.
   That splits the fault cleanly: if libinput sees nothing, it is below the
   compositor and nothing in this repo can be the cause.

Do **not** try to clean up by name-matching processes. `pkill cosmic-*` and
friends have twice killed this user's live desktop. Kill by a PID captured at
spawn, or a process group after `setsid`, and confirm with
`readlink /proc/<pid>/exe` first.
