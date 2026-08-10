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
            Action::ExtractArchive { kind, .. } | Action::CopyVerbatim { kind, .. } => *kind,
            Action::CopyWallpaper { .. } => AssetKind::Wallpaper,
        }
    }

    pub fn dest(&self) -> &Path {
        match self {
            Action::ExtractArchive { dest, .. }
            | Action::CopyWallpaper { dest, .. }
            | Action::CopyVerbatim { dest, .. } => dest,
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

        self.plan_wallpapers(theme_dir, theme_name, overwrite, &mut draft);

        for (kind, filename) in [
            (AssetKind::Waybar, "waybar.theme"),
            (AssetKind::Rofi, "rofi.theme"),
            (AssetKind::Kitty, "kitty.theme"),
        ] {
            self.plan_verbatim(theme_dir, kind, filename, overwrite, &mut draft);
        }

        draft.finish()
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

    fn plan_wallpapers(
        &self,
        theme_dir: &Path,
        theme_name: &str,
        overwrite: bool,
        draft: &mut Draft,
    ) {
        let wallpapers_dir = theme_dir.join("wallpapers");
        if !wallpapers_dir.is_dir() {
            return;
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
                return;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    draft.errors.push(e.into());
                    continue;
                }
            };
            let src = entry.path();
            if !src.is_file() {
                continue;
            }
            let dest = dest_dir.join(entry.file_name());
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
                Action::CopyVerbatim { dest, contents, .. } => {
                    if let Some(parent) = dest.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::write(dest, contents)?;
                    installed.push(dest.clone());
                }
            }
        }
        Ok(Report { installed })
    }
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
            .plan(&theme_dir, Some(&source_dir), "Evil", false)
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
            .plan(&theme_dir, Some(&source_dir), "Evil", false)
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
            .plan(&theme_dir, Some(&source_dir), "Mocha", false)
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
        let plan = installer.plan(&theme_dir, None, "Mocha", false).unwrap();
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
        let plan = installer.plan(&theme_dir, None, "Mocha", false).unwrap();
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
        let plan = installer.plan(&theme_dir, None, "Mocha", false).unwrap();
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
            .plan(&theme_dir, Some(&source_dir), "Mocha", false)
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
            .plan(&theme_dir, Some(&source_dir), "Mocha", false)
            .unwrap();

        assert!(
            plan.actions.is_empty(),
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
            .plan(&theme_dir, Some(&source_dir), "Mocha", true)
            .unwrap();
        assert_eq!(plan.actions.len(), 1);

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
        let plan = installer.plan(&theme_dir, None, "Mocha", false).unwrap();

        assert!(plan.actions.is_empty());
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
        let plan = installer.plan(&theme_dir, None, "Mocha", false).unwrap();
        assert_eq!(plan.actions.len(), 1);
        assert!(plan.skipped.is_empty());
    }

    #[test]
    fn report_lists_installed_and_skipped() {
        let tmp = TempDir::new().unwrap();
        let theme_dir = tmp.path().join("theme");
        write(&theme_dir.join("wallpapers/bg.png"), "x");
        write(&theme_dir.join("rofi.theme"), "not a header\n* {}\n");

        let home = tmp.path().join("home");
        let installer = Installer::with_paths(home.join(".local/share"), &home);
        let plan = installer.plan(&theme_dir, None, "Mocha", false).unwrap();
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
