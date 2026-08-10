//! Install the non-config parts of a HyDE theme: GTK/icon themes, wallpapers,
//! and the upstream `.theme` files that belong to waybar/rofi/kitty.
//!
//! `import.rs` handles `hypr.theme` -> `cosmic.conf`. Everything else in a
//! HyDE theme directory is either a foreign binary blob (a tarball) or a
//! config file for a program cosmic-conf does not own, so there is nothing to
//! translate — only to place correctly. As with `emit.rs`, installation is
//! two-stage: `plan` discovers what would happen without touching disk,
//! `apply` does the writing. A theme directory is untrusted input (it may
//! have been downloaded from anywhere), so the tarball extraction path is
//! hardened against path traversal.
//!
//! Real layout, verified against `HyDE-Project/hyde-themes`, branch
//! `Catppuccin-Mocha`:
//!
//! ```text
//! Configs/.config/hyde/themes/<Theme Name>/hypr.theme
//! Configs/.config/hyde/themes/<Theme Name>/waybar.theme
//! Configs/.config/hyde/themes/<Theme Name>/rofi.theme
//! Configs/.config/hyde/themes/<Theme Name>/kitty.theme
//! Configs/.config/hyde/themes/<Theme Name>/wallpapers/*
//! Source/Gtk_<Name>.tar.gz
//! Source/Icon_<Name>.tar.gz
//! ```
//!
//! The GTK/icon tarballs live in a `Source/` directory that is a sibling of
//! `Configs/`, not inside the per-theme folder, so `plan` takes it as a
//! separate optional argument rather than assuming it is nested under
//! `theme_dir`.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use flate2::read::GzDecoder;

#[derive(Debug)]
pub enum AssetError {
    Io(io::Error),
    /// A tarball entry (or, for a symlink/hardlink, its link target) with an
    /// absolute path or a `..` component. Both would let extraction write
    /// outside `dest_root`, so this is refused unconditionally rather than
    /// sanitised — a theme directory is untrusted input.
    UnsafeArchiveEntry {
        archive: PathBuf,
        entry: PathBuf,
    },
    NoHomeDirectory,
}

impl fmt::Display for AssetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AssetError::Io(e) => write!(f, "io error: {e}"),
            AssetError::UnsafeArchiveEntry { archive, entry } => write!(
                f,
                "refusing to extract `{}`: entry `{}` escapes the destination directory",
                archive.display(),
                entry.display()
            ),
            AssetError::NoHomeDirectory => write!(f, "no home directory available"),
        }
    }
}

impl std::error::Error for AssetError {}

impl From<io::Error> for AssetError {
    fn from(e: io::Error) -> Self {
        AssetError::Io(e)
    }
}

/// What kind of asset a `Note` or `Action` concerns, for report labelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetKind {
    Gtk,
    Icon,
    Wallpaper,
    Waybar,
    Rofi,
    Kitty,
}

impl AssetKind {
    fn label(self) -> &'static str {
        match self {
            AssetKind::Gtk => "GTK theme",
            AssetKind::Icon => "icon theme",
            AssetKind::Wallpaper => "wallpaper",
            AssetKind::Waybar => "waybar config",
            AssetKind::Rofi => "rofi config",
            AssetKind::Kitty => "kitty config",
        }
    }
}

/// Why an asset was left out of the plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// The destination already exists and `overwrite` was not requested.
    /// Never overwriting silently is the point — a stale install must be
    /// explicit, not a side effect of importing a new theme.
    AlreadyInstalled,
    /// The `.theme` file did not open with a `$HOME` destination header (see
    /// `split_hyde_header`), so there is no known upstream location to copy
    /// it to.
    NoDestinationHeader,
}

impl SkipReason {
    pub fn describe(&self) -> &'static str {
        match self {
            SkipReason::AlreadyInstalled => "already installed; pass --overwrite to replace it",
            SkipReason::NoDestinationHeader => {
                "no $HOME destination header; cannot determine an install path"
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub kind: AssetKind,
    pub path: PathBuf,
    pub reason: SkipReason,
}

/// One pending install step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Extract a GTK/icon tarball into `dest` (`~/.themes` or `~/.icons`).
    /// The tarball supplies its own top-level directory name.
    ExtractArchive {
        kind: AssetKind,
        archive: PathBuf,
        dest: PathBuf,
    },
    /// A wallpaper image, copied byte-for-byte.
    CopyWallpaper { src: PathBuf, dest: PathBuf },
    /// A waybar/rofi/kitty `.theme` file, copied with its HyDE destination
    /// header stripped (see `split_hyde_header`) but its body untouched.
    CopyVerbatim {
        kind: AssetKind,
        src: PathBuf,
        dest: PathBuf,
        contents: String,
    },
    /// A file this crate composes rather than copies. No `src`, because there
    /// is no upstream file: rofi's entry point and its per-machine overrides
    /// exist only because HyprCosmic needs them, and a HyDE theme has no
    /// equivalent to copy from.
    WriteGenerated {
        kind: AssetKind,
        dest: PathBuf,
        contents: String,
    },
    /// A stable name for the wallpaper in use, as a symlink beside the copies.
    ///
    /// HyDE has the same problem and solves it the same way: everything that
    /// wants to show the current wallpaper -- the launcher's sidebar, the
    /// autostart's `awww img` line -- needs one path that does not change when
    /// the theme does. HyDE points them at `~/.cache/hyde/wall.thmb`; this is
    /// that, under a name we own.
    LinkWallpaper { link: PathBuf, target: PathBuf },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub actions: Vec<Action>,
    pub skipped: Vec<Note>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub installed: Vec<PathBuf>,
}

/// A `Plan` under construction, plus the errors found while building it.
///
/// The three collections travel together through every `plan_*` helper, so
/// they are one parameter rather than three `&mut Vec`s. Errors accumulate
/// instead of returning early: a theme with one unreadable file should still
/// report what it would have done with the rest, the same way `resolve`
/// collects diagnostics rather than stopping at the first.
#[derive(Default)]
struct Draft {
    actions: Vec<Action>,
    skipped: Vec<Note>,
    errors: Vec<AssetError>,
}

impl Draft {
    fn finish(self) -> Result<Plan, Vec<AssetError>> {
        if self.errors.is_empty() {
            Ok(Plan {
                actions: self.actions,
                skipped: self.skipped,
            })
        } else {
            Err(self.errors)
        }
    }
}

impl Action {
    pub fn kind(&self) -> AssetKind {
        match self {
            Action::ExtractArchive { kind, .. }
            | Action::CopyVerbatim { kind, .. }
            | Action::WriteGenerated { kind, .. } => *kind,
            Action::CopyWallpaper { .. } | Action::LinkWallpaper { .. } => AssetKind::Wallpaper,
        }
    }

    pub fn dest(&self) -> &Path {
        match self {
            Action::ExtractArchive { dest, .. }
            | Action::CopyWallpaper { dest, .. }
            | Action::CopyVerbatim { dest, .. }
            | Action::WriteGenerated { dest, .. } => dest,
            Action::LinkWallpaper { link, .. } => link,
        }
    }
}

/// What `apply` would do, for `--dry-run`.
///
/// Deliberately not `render_report` with a synthesised `Report`: saying
/// "Installed:" about files that were never written is the kind of small lie
/// that makes a tool untrustworthy.
pub fn render_plan(plan: &Plan) -> String {
    let mut out = String::new();
    if plan.actions.is_empty() {
        out.push_str("Nothing to install.\n");
    } else {
        out.push_str("Would install:\n");
        for a in &plan.actions {
            out.push_str(&format!(
                "  {} ({})\n",
                a.dest().display(),
                a.kind().label()
            ));
        }
    }
    if !plan.skipped.is_empty() {
        out.push_str("\nWould skip:\n");
        for n in &plan.skipped {
            out.push_str(&format!(
                "  {} ({}): {}\n",
                n.path.display(),
                n.kind.label(),
                n.reason.describe()
            ));
        }
    }
    out
}

/// Human-readable summary, in the same spirit as `import::render_report`:
/// nothing that was skipped is left unmentioned.
pub fn render_report(plan: &Plan, report: &Report) -> String {
    let mut out = String::new();
    if report.installed.is_empty() {
        out.push_str("Nothing was installed.\n");
    } else {
        out.push_str("Installed:\n");
        for p in &report.installed {
            out.push_str(&format!("  {}\n", p.display()));
        }
    }
    if !plan.skipped.is_empty() {
        out.push_str("\nSkipped:\n");
        for n in &plan.skipped {
            out.push_str(&format!(
                "  {} ({}): {}\n",
                n.path.display(),
                n.kind.label(),
                n.reason.describe()
            ));
        }
    }
    out
}

pub struct Installer {
    /// `$XDG_DATA_HOME`, falling back to `$HOME/.local/share` — same
    /// fallback shape as `Emitter::from_env` (emit.rs:126-137). Wallpapers
    /// live under here.
    data_home: PathBuf,
    /// `$HOME` itself: GTK/icon themes go to the legacy `~/.themes` /
    /// `~/.icons` locations rather than anywhere under `data_home`, and it is
    /// also what a `.theme` file's `$HOME`-relative destination header
    /// expands against.
    home: PathBuf,
}

impl Installer {
    pub fn from_env() -> Result<Self, AssetError> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or(AssetError::NoHomeDirectory)?;
        let data_home = match std::env::var_os("XDG_DATA_HOME") {
            Some(x) if !x.is_empty() => PathBuf::from(x),
            _ => home.join(".local").join("share"),
        };
        Ok(Self { data_home, home })
    }

    pub fn with_paths(data_home: impl Into<PathBuf>, home: impl Into<PathBuf>) -> Self {
        Self {
            data_home: data_home.into(),
            home: home.into(),
        }
    }

    /// Discover every installable asset in a theme and decide where it would
    /// go, without writing anything. Mirrors `Emitter::plan` / `plan_one`:
    /// return every error rather than the first, and never touch the
    /// destination tree.
    ///
    /// `overwrite` controls what happens when a destination already exists —
    /// see `SkipReason::AlreadyInstalled`. `theme_name` names the wallpaper
    /// subdirectory (`$XDG_DATA_HOME/wallpapers/hyprcosmic/<theme_name>/`).
    pub fn plan(
        &self,
        theme_dir: &Path,
        source_dir: Option<&Path>,
        theme_name: &str,
        icon_theme: Option<&str>,
        overwrite: bool,
    ) -> Result<Plan, Vec<AssetError>> {
        let mut draft = Draft::default();

        if let Some(source_dir) = source_dir {
            for (kind, prefix, dest_root) in [
                (AssetKind::Gtk, "Gtk_", self.home.join(".themes")),
                (AssetKind::Icon, "Icon_", self.home.join(".icons")),
            ] {
                match find_tarball(source_dir, prefix) {
                    Ok(Some(archive)) => {
                        match self.plan_archive(kind, &archive, &dest_root, overwrite) {
                            Ok(Some(action)) => draft.actions.push(action),
                            Ok(None) => draft.skipped.push(Note {
                                kind,
                                path: archive,
                                reason: SkipReason::AlreadyInstalled,
                            }),
                            Err(e) => draft.errors.push(e),
                        }
                    }
                    Ok(None) => {} // No tarball of this kind — not every theme ships both.
                    Err(e) => draft.errors.push(e),
                }
            }
        }

        let wallpaper = self.plan_wallpapers(theme_dir, theme_name, overwrite, &mut draft);

        for (kind, filename) in [
            (AssetKind::Waybar, "waybar.theme"),
            (AssetKind::Rofi, "rofi.theme"),
            (AssetKind::Kitty, "kitty.theme"),
        ] {
            self.plan_verbatim(theme_dir, kind, filename, overwrite, &mut draft);
        }

        self.plan_rofi(icon_theme, wallpaper.as_deref(), overwrite, &mut draft);

        draft.finish()
    }

    /// Where the launcher and the autostart both look for the wallpaper.
    /// See `Action::LinkWallpaper`.
    fn current_wallpaper_link(&self) -> PathBuf {
        self.data_home
            .join("wallpapers")
            .join("hyprcosmic")
            .join("current")
    }

    /// rofi's entry point and its per-machine overrides.
    ///
    /// Unlike everything else here these are composed, not copied — a HyDE
    /// theme has no rofi config, only a palette, because HyDE supplies the
    /// layout from its own launcher script and we have no launcher script. See
    /// `config/rofi/config.rasi` for what the four-file import chain is doing.
    ///
    /// Both files are subject to the same already-installed rule as the rest:
    /// a re-import will not silently replace a `local.rasi` you have edited.
    /// That does mean switching themes needs `--overwrite` to take effect, but
    /// so does `theme.rasi` beside it, and one rule that always holds beats two
    /// that nearly do.
    fn plan_rofi(
        &self,
        icon_theme: Option<&str>,
        wallpaper_link: Option<&Path>,
        overwrite: bool,
        draft: &mut Draft,
    ) {
        let rofi_dir = self.home.join(".config").join("rofi");
        for (name, contents) in [
            ("config.rasi", CONFIG_RASI.to_string()),
            ("local.rasi", render_local_rasi(icon_theme, wallpaper_link)),
        ] {
            let dest = rofi_dir.join(name);
            if !overwrite && dest.exists() {
                draft.skipped.push(Note {
                    kind: AssetKind::Rofi,
                    path: dest,
                    reason: SkipReason::AlreadyInstalled,
                });
                continue;
            }
            draft.actions.push(Action::WriteGenerated {
                kind: AssetKind::Rofi,
                dest,
                contents,
            });
        }
    }

    fn plan_archive(
        &self,
        kind: AssetKind,
        archive: &Path,
        dest_root: &Path,
        overwrite: bool,
    ) -> Result<Option<Action>, AssetError> {
        // `write: false` — this only reads the tarball's headers to validate
        // and name-check it; extraction happens in `apply`.
        let entries = walk_archive(archive, dest_root, false)?;
        if !overwrite {
            let already_present = top_level_names(&entries)
                .into_iter()
                .any(|name| dest_root.join(name).exists());
            if already_present {
                return Ok(None);
            }
        }
        Ok(Some(Action::ExtractArchive {
            kind,
            archive: archive.to_path_buf(),
            dest: dest_root.to_path_buf(),
        }))
    }

    /// Returns the path of the stable `current` symlink when the theme has a
    /// wallpaper to point it at, so the caller can wire the launcher up to the
    /// same image.
    fn plan_wallpapers(
        &self,
        theme_dir: &Path,
        theme_name: &str,
        overwrite: bool,
        draft: &mut Draft,
    ) -> Option<PathBuf> {
        let wallpapers_dir = theme_dir.join("wallpapers");
        if !wallpapers_dir.is_dir() {
            return None;
        }
        let dest_dir = self
            .data_home
            .join("wallpapers")
            .join("hyprcosmic")
            .join(theme_name);

        let entries = match fs::read_dir(&wallpapers_dir) {
            Ok(e) => e,
            Err(e) => {
                draft.errors.push(e.into());
                return None;
            }
        };
        // Sorted, because one of these becomes the `current` symlink and
        // `read_dir` order is whatever the filesystem feels like. An arbitrary
        // choice is fine; an unrepeatable one is not -- re-running the import
        // would silently change the wallpaper.
        let mut sources = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    draft.errors.push(e.into());
                    continue;
                }
            };
            if entry.path().is_file() {
                sources.push(entry.file_name());
            }
        }
        sources.sort();

        let first = sources.first().map(|name| dest_dir.join(name));

        for name in &sources {
            let src = wallpapers_dir.join(name);
            let dest = dest_dir.join(name);
            if !overwrite && dest.exists() {
                draft.skipped.push(Note {
                    kind: AssetKind::Wallpaper,
                    path: src,
                    reason: SkipReason::AlreadyInstalled,
                });
                continue;
            }
            draft.actions.push(Action::CopyWallpaper { src, dest });
        }

        // The link is repointed even when every wallpaper was skipped as
        // already installed: the copies are theme-specific and unchanged, but
        // the link is global and has to follow the theme just imported.
        let target = first?;
        let link = self.current_wallpaper_link();
        draft.actions.push(Action::LinkWallpaper {
            link: link.clone(),
            target,
        });
        Some(link)
    }

    fn plan_verbatim(
        &self,
        theme_dir: &Path,
        kind: AssetKind,
        filename: &str,
        overwrite: bool,
        draft: &mut Draft,
    ) {
        let src = theme_dir.join(filename);
        if !src.is_file() {
            return; // Optional — not every theme carries all three.
        }
        let text = match fs::read_to_string(&src) {
            Ok(t) => t,
            Err(e) => {
                draft.errors.push(e.into());
                return;
            }
        };
        match split_hyde_header(&text, &self.home) {
            Some((dest, body)) => {
                if !overwrite && dest.exists() {
                    draft.skipped.push(Note {
                        kind,
                        path: src,
                        reason: SkipReason::AlreadyInstalled,
                    });
                } else {
                    draft.actions.push(Action::CopyVerbatim {
                        kind,
                        src,
                        dest,
                        contents: body.to_string(),
                    });
                }
            }
            None => draft.skipped.push(Note {
                kind,
                path: src,
                reason: SkipReason::NoDestinationHeader,
            }),
        }
    }

    /// Write the plan. Callers should `plan` first so failures — in
    /// particular an unsafe archive entry — surface before anything is
    /// written, matching `Emitter::apply`'s contract.
    pub fn apply(&self, plan: &Plan) -> Result<Report, AssetError> {
        let mut installed = Vec::new();
        for action in &plan.actions {
            match action {
                Action::ExtractArchive { archive, dest, .. } => {
                    walk_archive(archive, dest, true)?;
                    installed.push(dest.clone());
                }
                Action::CopyWallpaper { src, dest } => {
                    if let Some(parent) = dest.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::copy(src, dest)?;
                    installed.push(dest.clone());
                }
                Action::CopyVerbatim { dest, contents, .. }
                | Action::WriteGenerated { dest, contents, .. } => {
                    if let Some(parent) = dest.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::write(dest, contents)?;
                    installed.push(dest.clone());
                }
                Action::LinkWallpaper { link, target } => {
                    if let Some(parent) = link.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    // `symlink` fails with EEXIST rather than replacing, and
                    // `link.exists()` follows the link, so it answers false for
                    // a dangling one -- which is exactly the case a previous
                    // import leaves behind after its theme directory is gone.
                    // `symlink_metadata` asks about the link itself.
                    if fs::symlink_metadata(link).is_ok() {
                        fs::remove_file(link)?;
                    }
                    std::os::unix::fs::symlink(target, link)?;
                    installed.push(link.clone());
                }
            }
        }
        Ok(Report { installed })
    }
}

/// rofi's entry point, embedded from the repo rather than kept as a string
/// literal here so that it stays a real `.rasi` file: syntax-highlightable,
/// diffable, and editable without recompiling to see the result.
const CONFIG_RASI: &str = include_str!("../../config/rofi/config.rasi");

/// Compose `~/.config/rofi/local.rasi` — the last of the four imports in
/// `config.rasi`, holding the two things that depend on this machine rather
/// than on the theme file or on `/usr/share`.
///
/// Both parts are optional and each is simply left out when there is nothing
/// to say. An absent `configuration` block leaves rofi on its own icon theme;
/// an absent `dummywall` rule leaves the sidebar filled with `@main-bg` from
/// `rules.rasi`. Emitting a block with an empty value in either case would be
/// worse than emitting nothing, because rofi would honour it.
fn render_local_rasi(icon_theme: Option<&str>, wallpaper_link: Option<&Path>) -> String {
    let mut out = String::from(
        r#"/* Per-machine launcher settings for HyprCosmic.
 *
 * Generated by `cosmic-conf import-theme --assets`. It is the last of the four
 * imports in config.rasi, so anything here wins; it is also overwritten by the
 * next import run with --overwrite, so keep hand edits somewhere else.
 *
 * Two things belong in this file and nothing else does: values that name a path
 * or a package on this particular machine, which neither /usr/share/hyprcosmic
 * nor a HyDE theme file can know.
 */
"#,
    );

    match icon_theme.map(quote_rasi_string) {
        Some(theme) => out.push_str(&format!(
            r#"
/* A list, so the first one actually installed wins. The theme names the first;
 * Adwaita is the freedesktop baseline and is always present. */
configuration {{
    icon-theme: {theme}, "Adwaita";
}}
"#
        )),
        None => out.push_str("\n/* The theme names no icon theme, so rofi keeps its own. */\n"),
    }

    match wallpaper_link.map(|p| quote_rasi_string(&p.to_string_lossy())) {
        Some(link) => out.push_str(&format!(
            r#"
/* The sidebar image. rofi's second url() argument is the scaling mode: "height"
 * fills the panel vertically and crops the sides, which is how HyDE's style_1
 * uses its wallpaper thumbnail.
 *
 * This is a symlink, not one of the copies beside it, so that the launcher and
 * the autostart's `awww img` line can name the same path and stay in step
 * through a theme change. Repoint the link, not this file. */
dummywall {{
    background-image: url({link}, height);
}}
"#
        )),
        None => out.push_str(
            "\n/* The theme ships no wallpaper, so the sidebar stays a flat panel in\n \
             * the theme's background colour. */\n",
        ),
    }

    out
}

/// Quote a value for `.rasi`, which has no escape syntax worth relying on.
///
/// The icon theme name arrives from a downloaded theme file and so is
/// untrusted; a `"` in it would close the string early and turn the rest of
/// the generated file into whatever the theme author wanted. Dropping the
/// characters that could do that is enough here — every value this is used on
/// is a name or a path, where a quote or a newline is malformed input rather
/// than something to preserve.
fn quote_rasi_string(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .filter(|c| *c != '"' && *c != '\\' && !c.is_control())
        .collect();
    format!("\"{cleaned}\"")
}

/// Every HyDE `.theme` file — not just `hypr.theme` — opens with a
/// destination line for HyDE's own installer. Verified against
/// `HyDE-Project/hyde-themes`, branch `Catppuccin-Mocha`:
///
/// - `waybar.theme`: `$HOME/.config/waybar/theme.css|${scrDir}/wbarconfgen.sh`
/// - `rofi.theme`:   `$HOME/.config/rofi/theme.rasi` (no pipe at all)
/// - `kitty.theme`:  `$HOME/.config/kitty/theme.conf|killall -SIGUSR1 kitty`
///
/// Only the path before any `|` is a destination; the remainder is a
/// post-install hook (a script path or a shell command) that this crate does
/// not execute — running a command sourced from an untrusted theme would be
/// a code-execution hole, not a config install.
///
/// The header line itself is HyDE installer metadata, not part of the file
/// the target program reads (a bare path as the first line of `theme.rasi`
/// is not valid rofi syntax), so it is stripped from the returned body —
/// same call `import.rs::strip_hyde_header` makes for `hypr.theme`, just
/// exposed here because the destination is also needed, not only the body.
fn split_hyde_header<'a>(src: &'a str, home: &Path) -> Option<(PathBuf, &'a str)> {
    let mut lines = src.splitn(2, '\n');
    let first = lines.next()?;
    let path_part = first.split('|').next().unwrap_or(first).trim();
    let rest = path_part.strip_prefix("$HOME")?;
    let dest = home.join(rest.trim_start_matches('/'));
    Some((dest, lines.next().unwrap_or("")))
}

fn find_tarball(dir: &Path, prefix: &str) -> Result<Option<PathBuf>, AssetError> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(prefix) && name.ends_with(".tar.gz") {
            return Ok(Some(entry.path()));
        }
    }
    Ok(None)
}

/// Read (and, when `write` is true, extract) a `.tar.gz`, validating every
/// entry first. Shared between `plan` (`write: false`, a pure read used only
/// to name-check and reject unsafe archives early) and `apply`
/// (`write: true`), so the safety check cannot drift between the two paths.
fn walk_archive(
    archive_path: &Path,
    dest_root: &Path,
    write: bool,
) -> Result<Vec<PathBuf>, AssetError> {
    let file = fs::File::open(archive_path)?;
    let mut ar = tar::Archive::new(GzDecoder::new(file));
    let mut entries = Vec::new();

    for entry in ar.entries()? {
        let mut entry = entry?;
        let rel = entry.path()?.into_owned();
        reject_unsafe_path(archive_path, &rel)?;

        // A symlink/hardlink's own entry path can be safe while its target
        // still points outside `dest_root`; a later entry written "through"
        // that link would then land wherever the link points.
        //
        // The target cannot use the same rule as the entry path, though. Icon
        // themes are built almost entirely out of relative symlinks pointing
        // at sibling directories -- Tela ships thousands of
        // `../devices/network-wireless.svg` -- so rejecting every `..` would
        // reject every real icon theme. What matters is not whether the target
        // contains `..` but whether it still lands inside `dest_root` once
        // resolved, which is what `stays_within_root` decides.
        let entry_type = entry.header().entry_type();
        if matches!(entry_type, tar::EntryType::Symlink | tar::EntryType::Link) {
            if let Some(target) = entry.link_name()? {
                // tar resolves a symlink target against the link's own
                // directory, but a hardlink target against the archive root.
                let base = match entry_type {
                    tar::EntryType::Link => Path::new(""),
                    _ => rel.parent().unwrap_or(Path::new("")),
                };
                if !stays_within_root(base, &target) {
                    return Err(AssetError::UnsafeArchiveEntry {
                        archive: archive_path.to_path_buf(),
                        entry: target.into_owned(),
                    });
                }
            }
        }

        if write {
            let dest = dest_root.join(&rel);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            entry.unpack(&dest)?;
        }

        entries.push(rel);
    }

    Ok(entries)
}

/// Refuse anything that could escape `dest_root` once joined onto it: an
/// absolute path replaces the join outright, and a `..` component walks back
/// out of it. This is the hard security boundary — a theme directory is
/// untrusted input.
fn reject_unsafe_path(archive: &Path, entry: &Path) -> Result<(), AssetError> {
    let escapes = entry.is_absolute()
        || entry
            .components()
            .any(|c| matches!(c, Component::ParentDir));
    if escapes {
        return Err(AssetError::UnsafeArchiveEntry {
            archive: archive.to_path_buf(),
            entry: entry.to_path_buf(),
        });
    }
    Ok(())
}

/// Does `base/target` still land inside the root it started from?
///
/// Resolution is lexical on purpose. At plan time the destination tree does
/// not exist yet, so `canonicalize` has nothing to work with; and following
/// real symlinks during validation would open a TOCTOU window between the
/// check and the extraction. Counting depth over the joined components
/// answers the only question that matters without touching the filesystem.
fn stays_within_root(base: &Path, target: &Path) -> bool {
    if target.is_absolute() {
        return false;
    }
    let mut depth: isize = 0;
    for c in base.components().chain(target.components()) {
        match c {
            Component::Normal(_) => depth += 1,
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            Component::CurDir => {}
            // An absolute component anywhere replaces everything before it.
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    true
}

/// The first path component of every entry, skipping a leading `./` — used
/// only as a best-effort "is this archive already installed?" heuristic
/// (real GTK/icon tarballs unpack into a single named directory), not as a
/// security boundary.
fn top_level_names(entries: &[PathBuf]) -> BTreeSet<PathBuf> {
    entries
        .iter()
        .filter_map(|p| {
            p.components()
                .find(|c| !matches!(c, Component::CurDir))
                .map(|c| PathBuf::from(c.as_os_str()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use tempfile::TempDir;

    #[test]
    fn a_relative_symlink_into_a_sibling_directory_is_allowed() {
        // Regression: rejecting every `..` in a link target rejected every
        // real icon theme. Tela ships thousands of exactly this shape, and
        // Tokyo-Night's Icon_TelaPurple.tar.gz would not extract.
        assert!(stays_within_root(
            Path::new("Tela-purple-dark/16/panel"),
            Path::new("../devices/network-wireless.svg")
        ));
        assert!(stays_within_root(
            Path::new("Tela/22/apps"),
            Path::new("../../16/apps/firefox.svg")
        ));
    }

    #[test]
    fn a_relative_symlink_that_climbs_past_the_root_is_still_refused() {
        // One `..` too many is the whole attack, so the boundary is exact
        // rather than approximate.
        assert!(!stays_within_root(
            Path::new("Tela/16/panel"),
            Path::new("../../../../etc/passwd")
        ));
        assert!(!stays_within_root(Path::new(""), Path::new("../escape")));
        assert!(!stays_within_root(
            Path::new("a"),
            Path::new("../../escape")
        ));
        // Exactly back to the root is fine; one further is not.
        assert!(stays_within_root(Path::new("a/b"), Path::new("../../c")));
        assert!(!stays_within_root(
            Path::new("a/b"),
            Path::new("../../../c")
        ));
    }

    #[test]
    fn an_absolute_symlink_target_is_refused_however_it_is_spelled() {
        assert!(!stays_within_root(
            Path::new("a/b"),
            Path::new("/etc/passwd")
        ));
        assert!(!stays_within_root(Path::new("a/b"), Path::new("/")));
    }

    #[test]
    fn detours_that_end_up_back_inside_are_allowed() {
        // `a/b/../c` never leaves, so refusing it would be strictness with no
        // security value.
        assert!(stays_within_root(
            Path::new("theme/scalable"),
            Path::new("../scalable/./places/../apps/icon.svg")
        ));
    }

    /// Build a `.tar.gz` fixture programmatically so tests do not depend on
    /// binary blobs checked into the repo.
    fn make_tarball(dir: &Path, name: &str, entries: &[(&str, &[u8])]) -> PathBuf {
        let path = dir.join(name);
        let file = fs::File::create(&path).unwrap();
        let enc = GzEncoder::new(file, Compression::default());
        let mut builder = tar::Builder::new(enc);
        for (entry_path, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, entry_path, *data).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();
        path
    }

    /// Like `make_tarball`, but writes the entry path straight into the
    /// header bytes instead of going through `tar::Header::set_path`, which
    /// already refuses absolute paths and `..` *at write time* — exactly the
    /// gap `assets.rs` itself must close, since a hostile tarball is not
    /// necessarily built with this crate's own tooling.
    fn make_malicious_tarball(dir: &Path, name: &str, entry_path: &str, data: &[u8]) -> PathBuf {
        let path = dir.join(name);
        let file = fs::File::create(&path).unwrap();
        let enc = GzEncoder::new(file, Compression::default());
        let mut builder = tar::Builder::new(enc);

        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        let name_bytes = entry_path.as_bytes();
        header.as_mut_bytes()[..name_bytes.len()].copy_from_slice(name_bytes);
        header.set_cksum();
        builder.append(&header, data).unwrap();

        builder.into_inner().unwrap().finish().unwrap();
        path
    }

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    /// Every plan carries rofi's `config.rasi` and `local.rasi`, whatever the
    /// theme does or does not ship. Tests about one particular asset look past
    /// that scaffolding rather than counting it, so that adding another
    /// generated file later does not break assertions about wallpapers.
    fn theme_assets(plan: &Plan) -> Vec<&Action> {
        plan.actions
            .iter()
            .filter(|a| !matches!(a, Action::WriteGenerated { .. }))
            .collect()
    }

    fn generated(plan: &Plan, filename: &str) -> String {
        plan.actions
            .iter()
            .find_map(|a| match a {
                Action::WriteGenerated { dest, contents, .. }
                    if dest.file_name().unwrap() == filename =>
                {
                    Some(contents.clone())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("no generated {filename} in plan"))
    }

    #[test]
    fn path_traversal_tarball_is_rejected() {
        let tmp = TempDir::new().unwrap();
        let source_dir = tmp.path().join("Source");
        fs::create_dir_all(&source_dir).unwrap();
        // A GTK tarball whose entry escapes via `..` instead of staying under
        // its own top-level directory.
        make_malicious_tarball(&source_dir, "Gtk_Evil.tar.gz", "../evil.txt", b"pwned");

        let theme_dir = tmp.path().join("theme");
        fs::create_dir_all(&theme_dir).unwrap();

        let home = tmp.path().join("home");
        let installer = Installer::with_paths(home.join(".local/share"), &home);
        let err = installer
            .plan(&theme_dir, Some(&source_dir), "Evil", None, false)
            .expect_err("a path-traversal entry must be rejected, not silently extracted");
        assert!(
            err.iter()
                .any(|e| matches!(e, AssetError::UnsafeArchiveEntry { .. })),
            "{err:?}"
        );

        // Nothing must have been written, in `~/.themes` or anywhere else.
        assert!(!home.join(".themes").exists());
    }

    #[test]
    fn absolute_path_entry_is_also_rejected() {
        let tmp = TempDir::new().unwrap();
        let source_dir = tmp.path().join("Source");
        fs::create_dir_all(&source_dir).unwrap();
        make_malicious_tarball(&source_dir, "Icon_Evil.tar.gz", "/etc/passwd", b"pwned");

        let theme_dir = tmp.path().join("theme");
        fs::create_dir_all(&theme_dir).unwrap();
        let home = tmp.path().join("home");
        let installer = Installer::with_paths(home.join(".local/share"), &home);

        let err = installer
            .plan(&theme_dir, Some(&source_dir), "Evil", None, false)
            .unwrap_err();
        assert!(
            err.iter()
                .any(|e| matches!(e, AssetError::UnsafeArchiveEntry { .. })),
            "{err:?}"
        );
    }

    #[test]
    fn plan_does_not_touch_disk() {
        let tmp = TempDir::new().unwrap();
        let theme_dir = tmp.path().join("theme");
        write(&theme_dir.join("wallpapers/bg.png"), "not really a png");
        write(
            &theme_dir.join("rofi.theme"),
            "$HOME/.config/rofi/theme.rasi\n* { main-bg: #000; }\n",
        );

        let source_dir = tmp.path().join("Source");
        fs::create_dir_all(&source_dir).unwrap();
        make_tarball(
            &source_dir,
            "Gtk_Mocha.tar.gz",
            &[("Mocha/gtk.css", b"* {}")],
        );

        let home = tmp.path().join("home");
        let installer = Installer::with_paths(home.join(".local/share"), &home);
        let plan = installer
            .plan(&theme_dir, Some(&source_dir), "Mocha", None, false)
            .expect("a well-formed theme must plan cleanly");

        assert!(!plan.actions.is_empty());
        assert!(
            !home.exists(),
            "planning must not create anything under $HOME"
        );
    }

    #[test]
    fn wallpapers_are_copied() {
        let tmp = TempDir::new().unwrap();
        let theme_dir = tmp.path().join("theme");
        write(&theme_dir.join("wallpapers/bg.png"), "fake wallpaper bytes");

        let home = tmp.path().join("home");
        let data_home = home.join(".local/share");
        let installer = Installer::with_paths(&data_home, &home);
        let plan = installer
            .plan(&theme_dir, None, "Mocha", None, false)
            .unwrap();
        let report = installer.apply(&plan).unwrap();

        let dest = data_home.join("wallpapers/hyprcosmic/Mocha/bg.png");
        assert!(report.installed.contains(&dest));
        assert_eq!(fs::read_to_string(&dest).unwrap(), "fake wallpaper bytes");
    }

    #[test]
    fn verbatim_body_is_copied_unchanged() {
        // Real rofi.theme header + body, verified against
        // HyDE-Project/hyde-themes (Catppuccin-Mocha branch).
        let tmp = TempDir::new().unwrap();
        let theme_dir = tmp.path().join("theme");
        let body = "* {\n    main-bg: #11111be6;\n    main-fg: #cdd6f4ff;\n}\n";
        write(
            &theme_dir.join("rofi.theme"),
            &format!("$HOME/.config/rofi/theme.rasi\n{body}"),
        );

        let home = tmp.path().join("home");
        let installer = Installer::with_paths(home.join(".local/share"), &home);
        let plan = installer
            .plan(&theme_dir, None, "Mocha", None, false)
            .unwrap();
        let report = installer.apply(&plan).unwrap();

        let dest = home.join(".config/rofi/theme.rasi");
        assert!(report.installed.contains(&dest));
        // Byte-identical to the source *body* — only the HyDE installer
        // header line (not valid rofi syntax) is removed.
        assert_eq!(fs::read_to_string(&dest).unwrap(), body);
    }

    #[test]
    fn waybar_and_kitty_headers_with_pipes_still_resolve() {
        // waybar.theme and kitty.theme carry a post-install hook after `|`
        // that must be ignored, not executed.
        let tmp = TempDir::new().unwrap();
        let theme_dir = tmp.path().join("theme");
        write(
            &theme_dir.join("waybar.theme"),
            "$HOME/.config/waybar/theme.css|${scrDir}/wbarconfgen.sh\n@define-color bar-bg #000;\n",
        );
        write(
            &theme_dir.join("kitty.theme"),
            "$HOME/.config/kitty/theme.conf|killall -SIGUSR1 kitty\n## name: Mocha\n",
        );

        let home = tmp.path().join("home");
        let installer = Installer::with_paths(home.join(".local/share"), &home);
        let plan = installer
            .plan(&theme_dir, None, "Mocha", None, false)
            .unwrap();
        installer.apply(&plan).unwrap();

        assert_eq!(
            fs::read_to_string(home.join(".config/waybar/theme.css")).unwrap(),
            "@define-color bar-bg #000;\n"
        );
        assert_eq!(
            fs::read_to_string(home.join(".config/kitty/theme.conf")).unwrap(),
            "## name: Mocha\n"
        );
    }

    #[test]
    fn gtk_and_icon_tarballs_extract_to_the_legacy_directories() {
        let tmp = TempDir::new().unwrap();
        let theme_dir = tmp.path().join("theme");
        fs::create_dir_all(&theme_dir).unwrap();
        let source_dir = tmp.path().join("Source");
        fs::create_dir_all(&source_dir).unwrap();
        make_tarball(
            &source_dir,
            "Gtk_Mocha.tar.gz",
            &[("Mocha/gtk-3.0/gtk.css", b"* {}")],
        );
        make_tarball(
            &source_dir,
            "Icon_Tela.tar.gz",
            &[("Tela/index.theme", b"[Icon Theme]")],
        );

        let home = tmp.path().join("home");
        let installer = Installer::with_paths(home.join(".local/share"), &home);
        let plan = installer
            .plan(&theme_dir, Some(&source_dir), "Mocha", None, false)
            .unwrap();
        installer.apply(&plan).unwrap();

        assert_eq!(
            fs::read_to_string(home.join(".themes/Mocha/gtk-3.0/gtk.css")).unwrap(),
            "* {}"
        );
        assert_eq!(
            fs::read_to_string(home.join(".icons/Tela/index.theme")).unwrap(),
            "[Icon Theme]"
        );
    }

    #[test]
    fn existing_theme_is_skipped_without_overwrite() {
        let tmp = TempDir::new().unwrap();
        let theme_dir = tmp.path().join("theme");
        fs::create_dir_all(&theme_dir).unwrap();
        let source_dir = tmp.path().join("Source");
        fs::create_dir_all(&source_dir).unwrap();
        make_tarball(
            &source_dir,
            "Gtk_Mocha.tar.gz",
            &[("Mocha/gtk.css", b"new")],
        );

        let home = tmp.path().join("home");
        // Simulate a theme already installed under the name the tarball uses.
        write(&home.join(".themes/Mocha/gtk.css"), "old");

        let installer = Installer::with_paths(home.join(".local/share"), &home);
        let plan = installer
            .plan(&theme_dir, Some(&source_dir), "Mocha", None, false)
            .unwrap();

        assert!(
            theme_assets(&plan).is_empty(),
            "already-installed theme must not be re-planned"
        );
        assert_eq!(plan.skipped.len(), 1);
        assert_eq!(plan.skipped[0].reason, SkipReason::AlreadyInstalled);

        // Confirm the skip is honoured all the way through apply, and the
        // existing file is left untouched.
        installer.apply(&plan).unwrap();
        assert_eq!(
            fs::read_to_string(home.join(".themes/Mocha/gtk.css")).unwrap(),
            "old"
        );
    }

    #[test]
    fn overwrite_flag_forces_reinstall() {
        let tmp = TempDir::new().unwrap();
        let theme_dir = tmp.path().join("theme");
        fs::create_dir_all(&theme_dir).unwrap();
        let source_dir = tmp.path().join("Source");
        fs::create_dir_all(&source_dir).unwrap();
        make_tarball(
            &source_dir,
            "Gtk_Mocha.tar.gz",
            &[("Mocha/gtk.css", b"new")],
        );

        let home = tmp.path().join("home");
        write(&home.join(".themes/Mocha/gtk.css"), "old");

        let installer = Installer::with_paths(home.join(".local/share"), &home);
        let plan = installer
            .plan(&theme_dir, Some(&source_dir), "Mocha", None, true)
            .unwrap();
        assert_eq!(theme_assets(&plan).len(), 1);

        installer.apply(&plan).unwrap();
        assert_eq!(
            fs::read_to_string(home.join(".themes/Mocha/gtk.css")).unwrap(),
            "new"
        );
    }

    #[test]
    fn theme_file_without_a_destination_header_is_reported_not_dropped() {
        let tmp = TempDir::new().unwrap();
        let theme_dir = tmp.path().join("theme");
        write(&theme_dir.join("rofi.theme"), "* { main-bg: #000; }\n");

        let home = tmp.path().join("home");
        let installer = Installer::with_paths(home.join(".local/share"), &home);
        let plan = installer
            .plan(&theme_dir, None, "Mocha", None, false)
            .unwrap();

        assert!(theme_assets(&plan).is_empty());
        assert_eq!(plan.skipped.len(), 1);
        assert_eq!(plan.skipped[0].reason, SkipReason::NoDestinationHeader);
    }

    #[test]
    fn missing_optional_files_are_not_errors() {
        // A theme dir with only wallpapers and no waybar/rofi/kitty/tarballs
        // at all must still plan cleanly.
        let tmp = TempDir::new().unwrap();
        let theme_dir = tmp.path().join("theme");
        write(&theme_dir.join("wallpapers/bg.png"), "x");

        let home = tmp.path().join("home");
        let installer = Installer::with_paths(home.join(".local/share"), &home);
        let plan = installer
            .plan(&theme_dir, None, "Mocha", None, false)
            .unwrap();
        // The wallpaper copy and the `current` symlink that points at it.
        assert_eq!(theme_assets(&plan).len(), 2);
        assert!(plan.skipped.is_empty());
    }

    #[test]
    fn rofi_entry_point_is_written_even_for_a_theme_that_ships_nothing() {
        // config.rasi is what makes rofi read any of this: rofi loads no other
        // filename on its own. A theme with no rofi.theme still needs it, or
        // the launcher falls back to its stock grey.
        let tmp = TempDir::new().unwrap();
        let theme_dir = tmp.path().join("theme");
        fs::create_dir_all(&theme_dir).unwrap();

        let home = tmp.path().join("home");
        let installer = Installer::with_paths(home.join(".local/share"), &home);
        let plan = installer
            .plan(&theme_dir, None, "Mocha", None, false)
            .unwrap();
        installer.apply(&plan).unwrap();

        let written = fs::read_to_string(home.join(".config/rofi/config.rasi")).unwrap();
        assert!(written.contains("@import \"theme.rasi\""), "{written}");
        assert!(written.contains("@import \"local.rasi\""), "{written}");
    }

    #[test]
    fn local_rasi_carries_the_icon_theme_and_the_wallpaper_link() {
        let tmp = TempDir::new().unwrap();
        let theme_dir = tmp.path().join("theme");
        write(&theme_dir.join("wallpapers/bg.png"), "x");

        let home = tmp.path().join("home");
        let installer = Installer::with_paths(home.join(".local/share"), &home);
        let plan = installer
            .plan(
                &theme_dir,
                None,
                "Mocha",
                Some("Tela-circle-dracula"),
                false,
            )
            .unwrap();

        let local = generated(&plan, "local.rasi");
        assert!(
            local.contains(r#""Tela-circle-dracula", "Adwaita""#),
            "{local}"
        );
        // The stable link, not the copy: see `Action::LinkWallpaper`.
        assert!(local.contains("wallpapers/hyprcosmic/current"), "{local}");
        assert!(!local.contains("bg.png"), "{local}");
    }

    #[test]
    fn local_rasi_omits_what_the_theme_does_not_supply() {
        // An empty block would be worse than no block: rofi would honour an
        // empty icon-theme list, and an unset background-image is what leaves
        // the sidebar on the theme's own colour.
        let tmp = TempDir::new().unwrap();
        let theme_dir = tmp.path().join("theme");
        fs::create_dir_all(&theme_dir).unwrap();

        let home = tmp.path().join("home");
        let installer = Installer::with_paths(home.join(".local/share"), &home);
        let plan = installer
            .plan(&theme_dir, None, "Mocha", None, false)
            .unwrap();

        let local = generated(&plan, "local.rasi");
        assert!(!local.contains("icon-theme"), "{local}");
        assert!(!local.contains("background-image"), "{local}");
    }

    #[test]
    fn a_quote_in_an_icon_theme_name_cannot_break_out_of_the_string() {
        // The name comes from a theme file that may have been downloaded from
        // anywhere, and lands in a config rofi will execute nothing from but
        // will happily be reconfigured by.
        let tmp = TempDir::new().unwrap();
        let theme_dir = tmp.path().join("theme");
        fs::create_dir_all(&theme_dir).unwrap();

        let home = tmp.path().join("home");
        let installer = Installer::with_paths(home.join(".local/share"), &home);
        let plan = installer
            .plan(
                &theme_dir,
                None,
                "Mocha",
                Some("Tela\"; terminal: \"evil"),
                false,
            )
            .unwrap();

        let local = generated(&plan, "local.rasi");
        // The payload survives as text -- it is a name, and mangling it beyond
        // recognition would be its own bug -- but only as text: it stays inside
        // one quoted string, so `terminal` is never a property rofi sets.
        assert!(
            local.contains(r#""Tela; terminal: evil", "Adwaita";"#),
            "{local}"
        );
        assert!(
            !local
                .lines()
                .any(|l| l.trim_start().starts_with("terminal:")),
            "{local}"
        );
    }

    #[test]
    fn the_current_wallpaper_link_is_the_first_in_sorted_order() {
        // Arbitrary is fine; unrepeatable is not. `read_dir` order would make
        // re-importing the same theme change the wallpaper at random.
        let tmp = TempDir::new().unwrap();
        let theme_dir = tmp.path().join("theme");
        for name in ["zebra.png", "apple.png", "middle.png"] {
            write(&theme_dir.join("wallpapers").join(name), "x");
        }

        let home = tmp.path().join("home");
        let installer = Installer::with_paths(home.join(".local/share"), &home);
        let plan = installer
            .plan(&theme_dir, None, "Mocha", None, false)
            .unwrap();
        installer.apply(&plan).unwrap();

        let link = home.join(".local/share/wallpapers/hyprcosmic/current");
        assert_eq!(
            fs::read_link(&link).unwrap().file_name().unwrap(),
            "apple.png"
        );
    }

    #[test]
    fn the_wallpaper_link_is_repointed_rather_than_failing_on_a_stale_one() {
        // The second import is the interesting one: `symlink` refuses to
        // replace, and a link left dangling by a removed theme directory reads
        // as absent to `Path::exists`.
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let link = home.join(".local/share/wallpapers/hyprcosmic/current");
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(tmp.path().join("gone/old.png"), &link).unwrap();
        assert!(
            !link.exists(),
            "the stale link must be dangling for this test"
        );

        let theme_dir = tmp.path().join("theme");
        write(&theme_dir.join("wallpapers/new.png"), "x");

        let installer = Installer::with_paths(home.join(".local/share"), &home);
        let plan = installer
            .plan(&theme_dir, None, "Mocha", None, false)
            .unwrap();
        installer.apply(&plan).unwrap();

        assert_eq!(
            fs::read_link(&link).unwrap().file_name().unwrap(),
            "new.png"
        );
    }

    #[test]
    fn an_edited_local_rasi_is_not_replaced_without_overwrite() {
        let tmp = TempDir::new().unwrap();
        let theme_dir = tmp.path().join("theme");
        fs::create_dir_all(&theme_dir).unwrap();

        let home = tmp.path().join("home");
        write(&home.join(".config/rofi/local.rasi"), "/* mine */\n");

        let installer = Installer::with_paths(home.join(".local/share"), &home);
        let plan = installer
            .plan(&theme_dir, None, "Mocha", None, false)
            .unwrap();
        installer.apply(&plan).unwrap();

        assert_eq!(
            fs::read_to_string(home.join(".config/rofi/local.rasi")).unwrap(),
            "/* mine */\n"
        );
        assert!(plan
            .skipped
            .iter()
            .any(|n| n.path.ends_with("local.rasi") && n.reason == SkipReason::AlreadyInstalled));
    }

    #[test]
    fn report_lists_installed_and_skipped() {
        let tmp = TempDir::new().unwrap();
        let theme_dir = tmp.path().join("theme");
        write(&theme_dir.join("wallpapers/bg.png"), "x");
        write(&theme_dir.join("rofi.theme"), "not a header\n* {}\n");

        let home = tmp.path().join("home");
        let installer = Installer::with_paths(home.join(".local/share"), &home);
        let plan = installer
            .plan(&theme_dir, None, "Mocha", None, false)
            .unwrap();
        let report = installer.apply(&plan).unwrap();

        let text = render_report(&plan, &report);
        assert!(text.contains("Installed:"), "{text}");
        assert!(text.contains("bg.png"), "{text}");
        assert!(text.contains("Skipped:"), "{text}");
        assert!(text.contains("no $HOME destination header"), "{text}");
    }

    #[test]
    fn from_env_honours_xdg_data_home() {
        // Same fallback chain as `Emitter::from_env` (emit.rs), applied to
        // XDG_DATA_HOME instead of XDG_CONFIG_HOME.
        let prev = std::env::var_os("XDG_DATA_HOME");
        std::env::set_var("XDG_DATA_HOME", "/tmp/xdg-data-probe");
        let installer = Installer::from_env().unwrap();
        assert_eq!(installer.data_home, Path::new("/tmp/xdg-data-probe"));
        match prev {
            Some(v) => std::env::set_var("XDG_DATA_HOME", v),
            None => std::env::remove_var("XDG_DATA_HOME"),
        }
    }
}
