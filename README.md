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
- **It installs next to COSMIC rather than over it.** The binaries are
  `/usr/bin/hyprcosmic-comp`, `/usr/bin/hyprcosmic-session` and
  `/usr/bin/hyprcosmic-conf`, and nothing here writes a path the distribution
  owns. The stock COSMIC entry stays on the greeter's menu, served by the
  distribution's own binaries, so the day the HyDE session does not start is one
  logout away from a desktop that does. (On Debian, where COSMIC is not
  packaged, the `.deb` carries the desktop itself — see [Installing](#installing).)

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
- The install goes to `/usr/bin/hyprcosmic-comp`, alongside upstream's two
  `.ron` defaults files, which are carried unmodified. The distribution's
  `cosmic-comp` is left where it is, for the stock session to keep using.

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

The easiest route is a package. Every tag builds one for Fedora, Arch and Debian
and attaches it to a draft release; `workflow_dispatch` on **Packages** builds
them at any other time and leaves them as run artifacts.

```shell
sudo dnf install ./hyprcosmic-*.rpm          # Fedora
sudo pacman -U   ./hyprcosmic-*.pkg.tar.zst  # Arch
sudo dpkg -i     ./hyprcosmic_*_amd64.deb    # Debian
```

Nothing is removed and nothing conflicts. COSMIC is a dependency rather than a
casualty: the package installs `hyprcosmic-comp`, `hyprcosmic-session` and
`hyprcosmic-conf` beside the distribution's, and takes cosmic-settings, the
portal, the OSD and the rest from the distribution at the version it tested
them at. Log out and pick **HyprCosmic** from the greeter; pick **COSMIC** to go
back.

An earlier revision did take the `cosmic-*` names, and it could not be
installed. Its files collided with 25 distribution packages, and the only way to
satisfy that was to erase them — including cosmic-greeter, which on a Fedora
COSMIC install *is* the display manager. A desktop you can only try by removing
the desktop you would fall back to is not one worth shipping.

**Debian is the exception**, because COSMIC is not packaged there — no
`cosmic-session`, no `cosmic-comp`, in any suite. There is nothing to depend on
and nothing to install beside, so the `.deb` carries the whole desktop it
compiled and stands alone, and the greeter offers **HyprCosmic** only. The
Fedora and Arch packages ship this fork's three binaries and nothing else.

Building it yourself instead:

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

Note that `just install` is not what the packages do. It writes upstream's whole
desktop at upstream's names, so run against `/usr` on a machine that has COSMIC
packaged it will overwrite files your package manager owns. The packages are
built from this same tree and then reduced to this fork's own files and renamed;
that step lives in `.github/workflows/packages.yml`, not in the justfile, which
is upstream's. Stage to a directory and inspect it, or install a package.

`install` depends on `build`, which is upstream's arrangement and means `sudo
just install` compiles as root. That is inherited, not chosen; if you would
rather not, build into a staging root as your own user and copy it into place.

Then log out. `HyprCosmic` appears on the greeter's session menu next to
`COSMIC`; both work.

### Per-user setup

Nothing to do. The session seeds `~/.config` from `/usr/share/hyprcosmic/skel`
at login, copying only what is missing:

```
~/.config/hyprcosmic/autostart
~/.config/hyprcosmic/cosmic.conf
~/.config/hyprcosmic/waybar/style.css
~/.config/hyprcosmic/waybar/theme.css
~/.config/rofi/config.rasi
~/.config/rofi/theme.rasi
~/.config/rofi/local.rasi
```

An existing file is never touched, not even when the skeleton is newer — the
file wins, one way, and a login is not an invitation to edit your config. Two
things follow: your edits survive every login and upgrade, and deleting a file
is how you ask for the default back. `~/.cache/hyprcosmic/session.log` records
what was seeded.

The cost of that is real and worth stating: an upgrade that improves a shipped
default will not reach a file you already have. If a release changes something
you want — the `autostart` line that sets the wallpaper did change once — take
it deliberately, after reading what you would lose:

```shell
diff -u ~/.config/hyprcosmic/autostart /usr/share/hyprcosmic/skel/hyprcosmic/autostart
cp /usr/share/hyprcosmic/skel/hyprcosmic/autostart ~/.config/hyprcosmic/
```

`autostart` is the one that matters most, because it is what starts waybar, the
wallpaper daemon and `hyprcosmic-conf watch`. Until the session seeded it, a
machine that had never run HyprCosmic logged in to a bare compositor: running,
holding the display and accepting input, with nothing drawn on the screen and no
binding that opened anything.

`style.css` and `config.rasi` are per-user rather than shared for one reason:
each `@import`s a sibling holding the installed HyDE theme's palette, and a
relative `@import` resolves against the importing file. Those siblings —
`theme.css`, `theme.rasi` and `local.rasi` — arrive empty and are written by
`import-theme --assets`. They ship empty rather than not at all because a
missing `@import` is fatal to both consumers rather than a warning they skip:
GTK fails the entire stylesheet, and rofi reports the error in place of the
launcher.

Working from a git checkout, `just install` still places nothing in a home
directory — under `sudo` the only home directory it could see is root's — but it
does install the skeleton, so logging in seeds the same seven files.

Runtime dependencies of the shell itself are not COSMIC's and are not built
here: `waybar`, `rofi` (wayland build), `awww` (formerly `swww`), and a Nerd
Font for the glyphs the bar and the launcher draw with.

The wallpaper is `awww`'s job, and the image it draws is
`~/.local/share/wallpapers/hyprcosmic/current` — a symlink `import-theme
--assets` maintains. Before you have imported a theme there is no such link, so
`hyprcosmic-wallpaper` falls back to whatever the distribution ships,
`/usr/share/backgrounds/cosmic` first; the Fedora package recommends
`cosmic-wallpapers` so there is something there. With no `awww` installed it
says so on the session log and leaves the background alone, rather than waiting
for a daemon that is never coming.

The font is the one thing the packaging cannot do for you on Fedora, which has
no package that provides a Nerd Font at all: `texlive-inconsolata-nerd-font`
installs under `texmf-dist` and kitty's `SymbolsNerdFont` under
`/usr/lib64/kitty`, and fontconfig scans neither. Arch has
`ttf-nerd-fonts-symbols`. Otherwise, install one into your own font directory:

```shell
mkdir -p ~/.local/share/fonts/JetBrainsMonoNerdFont
# unpack JetBrainsMono.zip from github.com/ryanoasis/nerd-fonts/releases there
fc-cache -f
fc-list ":charset=e0b0" family | grep -i nerd   # non-empty: the glyphs resolve
```

Query the charset on its own, as above. Adding a family filter —
`":charset=e0b0:family=JetBrainsMono Nerd Font"` — reports nothing even when the
font does cover the codepoint, because the family string is a comma-separated
alias list (`JetBrainsMono Nerd Font,JetBrainsMono NF`). Without the font the
bar still works; every icon is a tofu box.

## Configuration

`~/.config/hyprcosmic/cosmic.conf`, in Hyprland's idiom, compiled into
`cosmic-config` by:

```shell
hyprcosmic-conf apply           # once
hyprcosmic-conf apply --diff    # show what would change, write nothing
hyprcosmic-conf watch           # recompile on every edit, for the whole session
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
hyprcosmic-conf import-theme ~/.config/hyde/themes/'Tokyo Night'/hypr.theme \
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

The two forks carry a `hyprcosmic.yml` of the same shape, each building on
Fedora, Debian and Arch and asserting that its install landed at upstream's
paths — and that nothing landed in the private `/usr/libexec/hyprcosmic/` this
fork used to use, which is the assertion that would otherwise rot quietly.

`packages.yml` in this repository builds installable packages for the same three
distributions: an RPM, a `.pkg.tar.zst` and a `.deb`, each compiled inside a
container of the distribution it targets so the sonames it records are the ones
the installing machine will have. It runs on tags and on demand, not on every
push — three full desktop builds is hours of runner time. Tags additionally open
a **draft** release with the packages attached; drafts rather than published,
because installing one of these replaces the machine's desktop.

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
