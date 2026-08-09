# HyprCosmic — Design

**Date:** 2026-08-09
**Status:** Approved for implementation

## Goal

Run a HyDE-style desktop on COSMIC's compositor: HyDE themes apply end to end — palette,
wallpaper, gaps, rounding, bar, launcher, notifications — with all configuration driven from a
single commented, version-controllable text file in Hyprland's idiom.

The user reviewed the cost of the full-rice target and chose it explicitly over the cheaper
palette-only option.

### What this actually is

HyprCosmic is **HyDE with cosmic-comp as the compositor**, not COSMIC restyled to look like HyDE.
COSMIC's own shell surface — cosmic-panel applets, the workspace overview, and cosmic-settings'
appearance controls — is replaced, not themed. This framing is the honest description and should
appear in the project README.

## Non-goals

- Bit-compatibility with Hyprland's config parser. Syntax is familiar; an existing
  `hyprland.conf` will not work, because COSMIC's key names and concepts differ throughout.
- Preserving cosmic-settings as a working appearance editor. Configuration is one-way: the file
  wins, and GUI edits are overwritten on next apply.
- Gradient window borders. COSMIC's `active_hint` is a solid hint with no gradient support, and
  adding one is out of scope.
- Matching Hyprland's window-management semantics (dwindle/master layouts). cosmic-comp's BSP
  tiler stays.

## Verified findings

Everything below was read from the tree at `/home/dingo/cosmic-epoch`, not assumed.

| Finding | Evidence |
|---|---|
| cosmic-comp is GPL-3.0-only; forking is permitted | `cosmic-comp/src/lib.rs:7` |
| `COSMIC_SESSION_SOCK` is optional — cosmic-comp runs standalone | `cosmic-comp/src/session.rs:76,90` |
| wlr-layer-shell is implemented (foreign bars can render) | `cosmic-comp/src/wayland/handlers/layer_shell.rs` |
| ext-session-lock is implemented | `cosmic-comp/src/wayland/handlers/session_lock.rs` |
| ext-workspace-v1 is implemented, plus a cosmic v2 extension | `cosmic-comp/src/wayland/protocols/workspace/ext.rs` |
| `zwlr_foreign_toplevel_management_v1` is **absent** — only ext-foreign-toplevel-list exists | grep across `cosmic-comp/src/`; `handlers/foreign_toplevel_list.rs` |
| Gaps are real and theme-driven, `(u32, u32)` | `layout/tiling/mod.rs:4305`, `layout/floating/mod.rs:1689` |
| Blur exists but is **client-requested** via `ext-background-effect`, not compositor rule | `handlers/background_effect.rs`, `backend/render/wayland/blur_effect.rs`, `shaders/blur_{downsample,upsample}.frag` |
| Shadow and rounded-corner shaders exist | `backend/render/shaders/{shadow,rounded_rectangle,rounded_outline}.frag` |
| Animation engine exists; durations hardcoded | `src/lib.rs:197,209`; `shell/workspace.rs:75` |
| Window rules cover **tiling exceptions only** | `cosmic-settings-daemon/config/src/window_rules/mod.rs:41` |
| Compositor config surface is ~20 flat fields | `cosmic-comp/cosmic-comp-config/src/lib.rs:71-105` |
| cosmic-config is a filesystem KV store, one file per key, sparse (only changed keys materialise) | `~/.config/cosmic/`, 136 files across ~25 components |
| cosmic-config live-reloads via inotify | `cosmic-comp/src/config/mod.rs:173,219,251` |
| cosmic-panel has **zero** CSS/stylesheet support; renders via iced | grep across `cosmic-panel/` |
| Upstream velocity: 46 commits in 30 days | `git log --since="30 days ago"` in cosmic-comp |

### What a HyDE theme actually contains

Measured from `HyDE-Project/hyde-themes`, branch `Catppuccin-Mocha` (25 files):

| File | Size | Contents |
|---|---|---|
| `hypr.theme` | 1,316 B | gaps 3/8, `rounding 10`, `border_size 2`, gradient borders, `blur {size 6, passes 3}`, GTK/icon theme names |
| `waybar.theme` | 358 B | 7 `@define-color` lines — **not** a stylesheet |
| `rofi.theme` | 320 B | ~7 colour variables |
| `kitty.theme` | 1,536 B | palette |
| GTK + icon tarballs | 4.6 MB | standard themes |
| wallpapers | ~95 MB | the bulk |

The theme is ~3.5 KB of text. The bespoke widget styling belongs to HyDE itself, not to any
individual theme — which is why running HyDE's own bar and launcher is the shortest path to
fidelity.

## Architecture

Three repositories. Upstream components not listed are consumed unmodified.

| Repo | Kind | Purpose |
|---|---|---|
| `hyprcosmic/cosmic-comp` | Fork (GPL-3.0) | Protocol patches, then blur/animation config |
| `hyprcosmic/cosmic-conf` | New (GPL-3.0) | Config compiler + theme importer |
| `hyprcosmic/hyprcosmic` | New meta | Submodule pins, session definition, docs |

Runtime composition:

| Layer | Component | Modified? |
|---|---|---|
| Compositor | `hyprcosmic/cosmic-comp` | Yes — patches A, B, then polish |
| Bar | waybar (upstream, MIT) | No — consumes HyDE config + CSS directly |
| Launcher | rofi (upstream) | No — HyDE `.rasi` works |
| Notifications | swaync (upstream) | No |
| Wallpaper | swww (upstream) | No — matches HyDE; `CosmicBackground` unused |
| Session | forked cosmic-session | Yes — gate `start_component` calls |
| Config | `cosmic-conf` | New |

## Phase 1 — `cosmic-conf`

A Rust binary that compiles one text file into the cosmic-config tree. It is a compiler, not a
daemon owning state: COSMIC components keep reading cosmic-config and keep live-reloading through
their existing `ConfigWatchSource`. Nothing in COSMIC learns about `cosmic.conf`.

### Units

| Unit | Responsibility | Depends on |
|---|---|---|
| `parser` | text → AST with byte spans. Sections, `$variables`, `source=`, `#` comments | — |
| `schema` | Declarative registry: conf key → cosmic-config target + type + validator + doc | — |
| `resolve` | AST + schema → typed values. Variable expansion, type/range checking, diagnostics | `parser`, `schema` |
| `emit` | Typed values → cosmic-config writes | `resolve`, `cosmic-config` |
| `watch` | inotify on the conf file and its includes → re-run pipeline | all |

`parser`, `schema` and `resolve` are pure and touch nothing COSMIC-specific, so the hard logic is
unit-testable without a compositor running. Only `emit` binds to `cosmic-config`, and it is the
only unit Phase 2 modifies when new keys land.

Entry points: `cosmic-conf apply` (one-shot, non-zero exit on error), `cosmic-conf watch`,
`cosmic-conf apply --diff` (show what would be overwritten).

### File format

Hyprland-style syntax, hand-written recursive-descent parser (~500 lines). Chosen over KDL and
TOML because the authoring experience is the product requirement; a better-engineered format that
feels wrong fails the goal.

```
$accent = rgb(6b9fed)
$gap    = 8

general {
    gaps_in     = $gap
    gaps_out    = $gap * 2
    autotile    = true
    active_hint = true
}

decoration {
    rounding = 10
}

theme {
    mode   = dark
    accent = $accent
}

bind = SUPER, Return, spawn, kitty
bind = SUPER, Q,      close

source = ~/.config/hyprcosmic/monitors.conf
```

### Schema registry

The mapping is not 1:1. Some conf keys own a whole cosmic-config value; others own one field
inside a composite RON value (`decoration.rounding` targets one radius among six in
`corner_radii`; `gaps_in`/`gaps_out` are two halves of one `(u32, u32)`).

```rust
enum Target {
    Direct    { component: &'static str, version: u8, key: &'static str },
    Projected { component: &'static str, version: u8, key: &'static str,
                path: &'static [&'static str] },
}

Entry {
    conf:     "general.gaps_in",
    // Fan-out: Dark and Light are separate cosmic-config components
    targets:  &[
        Projected { component: "com.system76.CosmicTheme.Dark.Builder",  version: 1,
                    key: "gaps", path: &["1"] },
        Projected { component: "com.system76.CosmicTheme.Light.Builder", version: 1,
                    key: "gaps", path: &["1"] },
    ],
    ty:       Ty::U32,
    validate: Some(range(0..=128)),
    doc:      "Gap between adjacent tiled windows, in px",
}
```

**Spike-corrected facts** (verified in `vendor/libcosmic`):

- `gaps: (u32, u32)` lives on `ThemeBuilder` (`cosmic-theme/src/model/theme.rs:895`), **not** `CosmicTk`.
  Component IDs at `theme.rs:17-26`. Default `(0, 8)`.
- Tuple order is **`(outer, inner)`** — so `gaps_out` is index `0` and `gaps_in` is index `1`.
- Dark and Light Builders are **separate components**, so one conf key fans out to two targets.
  `Entry` therefore carries `targets: &[Target]`, not a single target.
- `CosmicTk` (`libcosmic/src/config/mod.rs:14`, ID `com.system76.CosmicTk`) holds
  `icon_theme`, `interface_font`, `monospace_font`, `header_size`, `interface_density`,
  `show_minimize`, `show_maximize`, `apply_theme_global` — `icon_theme` is needed by the HyDE
  importer, which sets `$ICON_THEME`.

**`emit` writes through the typed `cosmic-config` API, not raw files.** `Config::watch`
(`cosmic-config/src/lib.rs:377`) is a `notify` inotify watch on the config directory that derives
changed keys from file paths, so raw writes would in fact be observed — but `Config::set` gives
correct RON encoding per type, atomic writes via `atomicwrites::AtomicFile` (`lib.rs:513`), and
matches the watcher's `.atomicwrite` temp-file filter (`lib.rs:408`). cosmic-conf therefore
depends on `cosmic-theme`, `cosmic-comp-config` and `cosmic-settings-config` for the concrete
types, which also buys compile-time type checking of the registry.

**Critical correctness property:** projected writes are read-modify-write, and multiple conf keys
can share one target. `emit` MUST group by target key, fold all projections, then write once.
Naïve per-key writes let `gaps_out` clobber `gaps_in`. This is directly unit-testable and is the
highest-value test in the suite.

`doc` generates `cosmic.conf.default`, so the annotated reference file cannot drift from the
schema.

### Phase 1 scope

| Section | Targets | Confidence |
|---|---|---|
| `general` | `CosmicComp`: autotile, active_hint, focus_follows_cursor(+delay), cursor_follows_focus, edge_snap_threshold, cursor_hide_timeout | Verified |
| `workspace` | `CosmicComp/workspaces`: mode, layout, wraparound, action_on_typing | Verified |
| `input` | `CosmicComp`: xkb_config, input_default, input_touchpad | Verified |
| `bind` | `CosmicSettings.Shortcuts/custom`, incl. `Spawn(String)` | Verified |
| `windowrule` | `WindowRules`: tiling exceptions only | Verified, deliberately thin |
| `theme` | `CosmicTheme.Mode/is_dark`, `.Builder/{palette,corner_radii,spacing}` | **Unverified** |
| `decoration` | `corner_radii`, `gaps` | **Unverified** |

### Spikes (must complete before schema work)

1. **Fetch libcosmic and enumerate `cosmic-theme` and `CosmicTk`.** `gaps` was inferred from its
   use site (`theme.cosmic().gaps`); the struct has not been read. If `gaps` is derived rather
   than stored, that row moves to Phase 2 and needs a compositor patch.
2. **Determine whether direct RON file writes trigger `ConfigWatchSource`,** or whether `emit`
   must go through the typed `cosmic-config` API. Decides `emit`'s implementation.

### Error handling

The pipeline is transactional: `resolve` fully validates before `emit` writes anything. A
malformed file leaves the desktop untouched rather than half-applied. Diagnostics report against
source text with spans:

```
error: unknown key `gaps_inn` in section `general`
  --> cosmic.conf:7:5
   |
 7 |     gaps_inn = 8
   |     ^^^^^^^^ did you mean `gaps_in`?
```

## Phase 2 — cosmic-comp patches

Ordered by ascending risk. Each is independently shippable. Patches A and B are additive new
files that never touch `shell/layout/tiling/mod.rs` — a 235 KB file that is the most painful
thing in the tree to carry patches against.

### Patch A — `zwlr_foreign_toplevel_management_v1`

New protocol handler alongside `toplevel_info.rs` / `toplevel_management.rs`, which already hold
the required state. Unlocks waybar's `wlr/taskbar`. Plausibly upstreamable. **Rebase risk: low.**

### Patch B — Hyprland-compatible IPC socket

Implement a subset of Hyprland's IPC at
`$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket` (request/response) and `.socket2`
(event stream).

- Requests: `workspaces`, `activeworkspace`, `activewindow`, `clients`, `monitors`
- Events: `workspace>>`, `activewindow>>`, `openwindow>>`, `closewindow>>`

HyDE's `hyprland/workspaces` and `hyprland/window` waybar modules then work unmodified, because
waybar cannot tell the difference. Also delivers the `hyprctl`-style IPC from the original
wishlist. New file, no entanglement with the layout engine. **Rebase risk: low.**

### Polish patches

| Patch | Where | Effort | Rebase risk |
|---|---|---|---|
| Animation curves + durations | `shell/`, config struct | Medium | Low — replaces consts with config lookups |
| Opacity + shadow config | `backend/render/`, `shadow.frag` | Medium | Low — shader exists, needs uniforms |
| Per-monitor/workspace gaps | both layout modules | Low | Low |
| Compositor-driven blur rules | `backend/render/wayland/blur_effect.rs` | High | Medium — inverts client-request model |
| Real window rules | `shell/layout/tiling/mod.rs` | High | **High** — do last |

## Phase 3 — Session and theme importer

### Session

Fork cosmic-session and gate the hardcoded `start_component` calls (cosmic-panel,
cosmic-launcher, cosmic-app-library, cosmic-osd, cosmic-workspaces) behind config. Forking is
preferred over skipping cosmic-session entirely, because cosmic-session also propagates the
compositor environment to systemd/D-Bus and pulls up `graphical-session.target`; without it,
portals and D-Bus-activated apps break.

cosmic-greeter is retained. Display-manager changes are the easiest way to lose access to a
machine.

Ships a `hyprcosmic.desktop` session entry **alongside** the existing COSMIC session, so the
working desktop remains selectable at login throughout development.

### Theme importer

`cosmic-conf import-theme <path-or-hyde-branch>` — one-way into `cosmic.conf`, not straight into
cosmic-config, so the result is readable and editable.

1. Parse `hypr.theme` with the Phase 1 parser (same grammar — this is where the syntax choice pays off)
2. Map recognised keys through a translation table into HyprCosmic conf keys
3. Extract the palette; derive COSMIC's palette from border/accent colours via the Builder's
   tinting inputs (`neutral_tint`, `accent`, `bg_color`)
4. Install GTK/icon tarballs, register wallpapers
5. Copy `waybar.theme`, `rofi.theme`, `kitty.theme` to their upstream destinations unmodified
6. **Emit an explicit unsupported-keys report** rather than silently dropping — e.g.
   `col.active_border: gradient not supported (COSMIC active_hint is solid)`

The report is the honesty mechanism that keeps partial import from feeling broken.

## Rejected alternatives

| Alternative | Why rejected |
|---|---|
| **caffyne-shell as the shell** | Python/GTK3 (93% Python), **no license file** (all rights reserved), 10 weeks old at evaluation. Protocol prerequisites were verified present in cosmic-comp, so this remains technically viable if the license is resolved. |
| **CSS theming inside cosmic-panel** | Requires building a CSS cascade for a retained-mode iced UI. Large new subsystem, and the result still would not consume HyDE's `style.css` verbatim. |
| **Teach cosmic-comp to read `cosmic.conf` natively** | The config surface spans ~25 components; a file parsed inside the compositor could only configure the compositor. Also the largest fork and breaks cosmic-settings outright. |
| **Bidirectional config sync** | Round-tripping a commented file through a KV store reliably is hard; failure mode is silently mangling the user's file. |
| **Patch cosmic-comp before building the config layer** | Nothing usable until late, and the fork would be driven by 136 individual files in the meantime. |

## Risks

| Risk | Mitigation |
|---|---|
| Upstream velocity (46 commits/30 days) makes rebasing costly | Keep patches additive and in new files; defer window rules; upstream Patch A if accepted |
| Theme/decoration schema rows are unverified | Spike 1 gates schema work; rows move to Phase 2 if `gaps` proves derived |
| Losing COSMIC's shell removes the appearance GUI | Accepted and documented; `--diff` makes one-way overwrites visible |
| Compositor work is not verifiable without a real session | Phase 1 is fully testable headless; Phases 2–3 need a nested or TTY session |
| Naming leans on two projects' marks | Personal fork is fine; rename before any wide distribution |

## Success criteria

1. `cosmic-conf apply` compiles a `cosmic.conf` into cosmic-config and COSMIC live-reloads it.
2. A malformed conf produces a spanned diagnostic and writes nothing.
3. `gaps_in` and `gaps_out` both land — proving projection folding works.
4. waybar runs on cosmic-comp showing workspaces and active window via Patch B, unmodified HyDE config.
5. `import-theme Catppuccin-Mocha` yields a matching palette, wallpaper, rounding and gaps, plus
   an accurate unsupported-keys report.
6. `hyprcosmic.desktop` is selectable at login and the stock COSMIC session still works.
