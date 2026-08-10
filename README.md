# HyprCosmic

COSMIC's compositor, driven the way Hyprland is configured, wearing a HyDE
shell.

It is a fork of [cosmic-epoch](https://github.com/pop-os/cosmic-epoch), the
meta-repository that names every COSMIC component and builds the desktop out of
them. Two of its 29 submodules point at forks; the other 27 are System76's,
unchanged. So this is not a re-implementation of COSMIC and not a theme pack
sitting beside it — it is COSMIC, built from source, with a different shell on
top and a different way of telling it what to do.

Three things distinguish a HyprCosmic session from a COSMIC one:

- **Hyprland's configuration idiom.** A single `~/.config/hyprcosmic/cosmic.conf`
  with `general { }` blocks, `bind =` lines and `$variables` is compiled into
  COSMIC's config tree. The file wins: what it names, it owns.
- **HyDE's shell.** waybar instead of cosmic-panel, rofi instead of
  cosmic-launcher, `awww` instead of cosmic-bg. HyDE themes are imported
  directly, palette and wallpapers and all.
- **It installs beside COSMIC, not over it.** The two modified binaries go to
  `/usr/libexec/hyprcosmic/`, and the session gets its own entry on the
  greeter's menu. A machine with COSMIC from its distribution keeps that session
  working, which matters on the day this one does not start.

## Repository layout

Everything in `cosmic-epoch`, plus:

| Path | What it is |
| --- | --- |
| `cosmic-comp/` | submodule → [outbackdingo/hyprcosmic-comp](https://github.com/outbackdingo/hyprcosmic-comp) |
| `cosmic-session/` | submodule → [outbackdingo/hyprcosmic-session](https://github.com/outbackdingo/hyprcosmic-session) |
| `cosmic-conf/` | the config compiler and HyDE theme importer. A crate in this repository, not a submodule |
| `config/` | the shipped `cosmic.conf`, `autostart`, waybar and rofi assets, and the power menu |
| `tools/install-assets.sh` | installs the parts of `config/` that live outside `$HOME`, and `--check`s them for drift |
| `docs/` | the design spec, a debugging guide, and one written-up bug that is still open |

The other 27 submodules stay on `pop-os`. Nothing about them needs to change,
and pinning them to copies nobody maintains would be a promise to keep 27 forks
current.

### What the two forks change

**cosmic-comp** — four patches, each independent:

- `zwlr_foreign_toplevel_management_v1`, which is the protocol waybar's window
  list and rofi's window mode read. Without it the taskbar is empty.
- A Hyprland-compatible IPC socket (`.socket.sock` and the `.socket2.sock` event
  stream) under the names Hyprland clients actually open, so HyDE's scripts and
  waybar's `hyprland/*` modules work unmodified. The write surface is
  deliberately small: `dispatch exec` and `dispatch killactive` are rejected,
  because this is the surface any process that can open the socket gets.
- New windows open *beside* the focused window rather than inside it.
- The install goes to `/usr/libexec/hyprcosmic/cosmic-comp`, and the shortcut
  defaults file is left alone.

**cosmic-session** — profiles. `HYPRCOSMIC_PROFILE=hyprcosmic` (set by
`hyprcosmic.desktop`) skips cosmic-panel, cosmic-launcher, cosmic-app-library,
cosmic-workspaces, cosmic-bg and cosmic-files-applet, then starts whatever
`~/.config/hyprcosmic/autostart` names. cosmic-greeter is deliberately *not*
skippable — a display manager is the easiest thing to lock yourself out of. The
fork installs three files where upstream installs seven; the four it drops are
owned by the distribution's own `cosmic-session` package and writing them would
make the two conflict.

## Building

```shell
git clone --recurse-submodules https://github.com/outbackdingo/hyprcosmic
cd hyprcosmic
just build
```

Build dependencies are COSMIC's — see [upstream's list](https://github.com/pop-os/cosmic-epoch#setup-on-distributions-without-packaging-of-cosmic-components),
which is long and distribution-specific. `rustup` is recommended over the
distribution's rustc: `cosmic-comp` is edition 2024 and pins Rust 1.93 in its
`rust-toolchain.toml`, which is newer than several stable distributions ship —
Debian bookworm's rustc is 1.63. `just` is likewise absent before Debian
trixie; `cargo install just --locked` covers it.

## Installing

```shell
sudo just install '' /usr
```

The two positional arguments are `rootdir` (a staging root, for packaging) and
`prefix`. **Use `/usr`, not the `/usr/local` default.** Several files name
`/usr/share/hyprcosmic` as a literal because they have no way to interpolate a
prefix — a rofi `.rasi` has no variables, `hyprcosmic.desktop` has no way to
expand one into `Exec=`, and `autostart` is deliberately not a shell.
`install-assets.sh` prints the exact list when you use another prefix.

To stage instead of install:

```shell
just install /tmp/stage /usr
```

This installs all of COSMIC — the 27 unmodified components as well — plus
`cosmic-conf` at `$prefix/bin/cosmic-conf`, the shared waybar and rofi assets
under `$prefix/share/hyprcosmic/`, and `hyprcosmic-powermenu`.

`install` depends on `build`, which is upstream's arrangement and means `sudo
just install` compiles as root. That is inherited, not chosen; if you would
rather not, build into a staging root as your own user and copy it into place.

Then log out. `HyprCosmic` appears on the greeter's session menu next to
`COSMIC`; both work.

### Per-user setup

`just install` places nothing in a home directory — under `sudo` the only home
directory it could see is root's. Four files are yours to place:

```shell
mkdir -p ~/.config/hyprcosmic/waybar
cp config/cosmic.conf config/autostart ~/.config/hyprcosmic/
cp config/waybar/style.css ~/.config/hyprcosmic/waybar/
```

`style.css` is per-user rather than shared for one reason: it `@import`s a
sibling `theme.css` holding the installed HyDE theme's palette, and a relative
`@import` resolves against the importing file. That sibling is written by
`import-theme --assets`, so the bar is unstyled until you have imported a theme.

The fourth file, `~/.config/rofi/config.rasi`, is written by `import-theme
--assets` too, because it names per-machine paths.

Runtime dependencies of the shell itself are not COSMIC's and are not built
here: `waybar`, `rofi` (wayland build), `awww` (formerly `swww`), and a Nerd
Font for the bar's glyphs.

## Configuration

`~/.config/hyprcosmic/cosmic.conf`, in Hyprland's idiom, compiled into
`cosmic-config` by:

```shell
cosmic-conf apply           # once
cosmic-conf apply --diff    # show what would change, write nothing
cosmic-conf watch           # recompile on every edit, for the whole session
```

`watch` is the first line of the shipped `autostart`, which is what makes "the
file wins" true at login and not only when you last ran `apply` by hand:
whatever COSMIC's settings UI stored since then is overwritten before the
desktop settles. A malformed edit is reported to the session log and the last
good configuration stays in place, so a typo cannot leave you at a broken
desktop.

The rule is one-way and deliberate. Keys this file names are overwritten from
it on every login; keys it does not name are left entirely alone, so
cosmic-settings remains the right place to change anything the file is silent
about. There is no write-back — the GUI never edits `cosmic.conf`.

`bind` lines go to the Shortcuts `custom` key, which cosmic-comp merges over
`defaults`, so the system defaults file is never touched and reverting is a
matter of deleting the lines and re-applying. Hyprland spellings and COSMIC
spellings are both accepted for the same setting (`input:follow_mouse` and
`general:focus_follows_cursor`), and the last assignment wins. Where a Hyprland
value has no COSMIC equivalent — `follow_mouse = 2` and `3`, which separate
pointer focus from keyboard focus — it is rejected with an explanation rather
than quietly rounded.

What the shipped file sets up, since the components those keys used to reach are
no longer running:

| Binding | Does |
| --- | --- |
| `Super` (tap), `Super+/`, `Super+A` | `rofi -show drun` |
| `Super+W` | `rofi -show window`, in place of the workspace overview |
| `Super+Return` | `cosmic-term` (`Super+T` still works — cosmic-comp handles that one itself) |
| `Super+Shift+E` | `hyprcosmic-powermenu`: lock, suspend, log out, reboot, shut down |

The power menu is there because cosmic-panel hosts COSMIC's power applet, and
without the panel a session had no way out short of `systemctl reboot` from a
terminal. The same script backs waybar's power button, so the two cannot drift
apart, and it confirms before anything that ends the session.

See [`config/cosmic.conf`](config/cosmic.conf); it is commented at length and is
the reference for what is supported.

## Theming

```shell
cosmic-conf import-theme ~/.config/hyde/themes/'Tokyo Night'/hypr.theme \
    --out ~/.config/hyprcosmic/theme.conf --report --assets
```

This translates a HyDE theme into conf keys, and with `--assets` also installs
the wallpapers, GTK and icon themes, and the waybar/rofi/kitty theme files that
sit beside `hypr.theme`. `--report` prints everything that did not translate
cleanly, which is the honest half of the output.

`theme.conf` is written as a separate file and `source`d from `cosmic.conf`
rather than pasted into it. That keeps re-importing from touching your
keybindings, and anything you want to override can simply be repeated later in
`cosmic.conf`, since the last assignment to a key wins. The `source` line ships
commented out — a `source` naming a file that does not exist is a hard error,
and no theme is imported on a fresh install. Uncomment it once you have run the
command above; `import-theme` says so as well.

Change the wallpaper by repointing the `current` symlink that `--assets`
maintains, not by editing `autostart`:

```shell
ln -sfn ~/".local/share/wallpapers/hyprcosmic/<theme>/<image>" \
        ~/.local/share/wallpapers/hyprcosmic/current
```

## Continuous integration

Two workflows, on purpose:

- `.github/workflows/ci.yml` is upstream's, unmodified. It builds the entire
  desktop on Arch via `just sysext`, which is exactly the check a meta-repo
  wants and is not made less useful by forking.
- `.github/workflows/hyprcosmic.yml` covers what upstream's does not:
  `cosmic-conf` built, tested and clippy-clean on Fedora, Debian and Arch; a
  check that the shipped `cosmic.conf` still parses and resolves against the
  current schema; that `config/waybar/config.jsonc` is still in step with the
  generator that produces it; that the template and generator stay pure ASCII;
  and an `install-assets.sh` round trip into a staging root, verified with
  `--check`.

The two forks carry a `hyprcosmic.yml` of the same shape, each asserting that
its install landed in `/usr/libexec/hyprcosmic/` and that the files owned by the
distribution's COSMIC packages went unwritten.

The waybar generator deserves its own note. `config.jsonc` is generated from
`config.jsonc.in` and a codepoint table in `generate-config.py`, and is never
hand-edited: Nerd Font glyphs live in the Private Use Area, where they are
destroyed by being retyped and indistinguishable from each other in a diff. CI
regenerates the file and fails if it moves.

## Known gaps

- The `/usr/share/hyprcosmic` literals described under [Installing](#installing).
- `docs/unreproducible-dead-input-2026-08-10.md` records a session that came up
  without input and has not been reproduced since. It is written down rather
  than closed.

## Trademark

COSMIC is a System76 trademark. This fork is not affiliated with or endorsed by
System76. See [TRADEMARK.md](TRADEMARK.md), which is upstream's policy and
applies here.

## Upstream

For COSMIC itself — the component list, packaging status, translations, and how
to install it on your distribution rather than building it — see
[pop-os/cosmic-epoch](https://github.com/pop-os/cosmic-epoch).
