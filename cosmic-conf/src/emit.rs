//! Resolved writes -> the cosmic-config tree.
//!
//! Spike 2 established the mechanism (see the spec's verified-findings table):
//! cosmic-config is a filesystem key-value store at
//! `$XDG_CONFIG_HOME/cosmic/<component>/v<n>/<key>`, each file holding one RON
//! literal. `Config::watch` (`cosmic-config/src/lib.rs:377`) is a `notify`
//! inotify watch on that directory which derives changed keys from file paths,
//! so a plain atomic write is observed exactly like a write from the typed API.
//! That is why this module needs `ron` rather than the whole libcosmic graph.
//!
//! Emission is two-stage on purpose. `plan` reads current state and renders
//! every file's new contents without touching disk; `apply` then writes. A
//! failure while planning therefore leaves the desktop untouched, preserving
//! the transactional guarantee `resolve` starts.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::resolve::{Resolved, TargetKey, Value, Write, WriteKind};

/// Mirror of `cosmic_theme::CornerRadii` (`cosmic-theme/src/model/corner.rs:5`).
///
/// Duplicated rather than depended upon so this crate stays free of the
/// libcosmic build graph. The field set and defaults are pinned by tests; if
/// upstream adds a radius, round-tripping would silently drop it, so
/// `deny_unknown_fields` turns that into a loud parse error instead.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CornerRadii {
    radius_0: [f32; 4],
    radius_xs: [f32; 4],
    radius_s: [f32; 4],
    radius_m: [f32; 4],
    radius_l: [f32; 4],
    radius_xl: [f32; 4],
}

impl Default for CornerRadii {
    /// `corner.rs:20-31`.
    fn default() -> Self {
        Self {
            radius_0: [0.0; 4],
            radius_xs: [4.0; 4],
            radius_s: [8.0; 4],
            radius_m: [16.0; 4],
            radius_l: [32.0; 4],
            radius_xl: [160.0; 4],
        }
    }
}

impl CornerRadii {
    fn field_mut(&mut self, name: &str) -> Option<&mut [f32; 4]> {
        Some(match name {
            "radius_0" => &mut self.radius_0,
            "radius_xs" => &mut self.radius_xs,
            "radius_s" => &mut self.radius_s,
            "radius_m" => &mut self.radius_m,
            "radius_l" => &mut self.radius_l,
            "radius_xl" => &mut self.radius_xl,
            _ => return None,
        })
    }
}

#[derive(Debug)]
pub enum EmitError {
    Io(io::Error),
    /// A projected target whose composite shape this emitter cannot rebuild.
    UnsupportedComposite {
        key: String,
        detail: String,
    },
    /// An existing file could not be parsed, so read-modify-write is unsafe.
    Unreadable {
        path: PathBuf,
        detail: String,
    },
    NoConfigDirectory,
}

impl fmt::Display for EmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EmitError::Io(e) => write!(f, "io error: {e}"),
            EmitError::UnsupportedComposite { key, detail } => {
                write!(f, "cannot write `{key}`: {detail}")
            }
            EmitError::Unreadable { path, detail } => {
                write!(f, "cannot parse existing `{}`: {detail}", path.display())
            }
            EmitError::NoConfigDirectory => write!(f, "no config directory available"),
        }
    }
}

impl std::error::Error for EmitError {}

impl From<io::Error> for EmitError {
    fn from(e: io::Error) -> Self {
        EmitError::Io(e)
    }
}

/// One file's worth of pending change. `previous` powers `apply --diff`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    pub path: PathBuf,
    pub contents: String,
    pub previous: Option<String>,
}

impl Planned {
    /// A write that would not change anything on disk.
    pub fn is_noop(&self) -> bool {
        self.previous.as_deref() == Some(self.contents.as_str())
    }
}

pub struct Emitter {
    root: PathBuf,
}

impl Emitter {
    /// Locate the cosmic-config root the same way cosmic-config does:
    /// `$XDG_CONFIG_HOME/cosmic`, falling back to `$HOME/.config/cosmic`.
    pub fn from_env() -> Result<Self, EmitError> {
        let base = match std::env::var_os("XDG_CONFIG_HOME") {
            Some(x) if !x.is_empty() => PathBuf::from(x),
            _ => {
                let home = std::env::var_os("HOME").ok_or(EmitError::NoConfigDirectory)?;
                PathBuf::from(home).join(".config")
            }
        };
        Ok(Self {
            root: base.join("cosmic"),
        })
    }

    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path_for(&self, target: &TargetKey) -> PathBuf {
        self.root
            .join(&target.component)
            .join(format!("v{}", target.version))
            .join(&target.key)
    }

    /// Render every write without touching disk.
    ///
    /// Returns all errors rather than the first, matching `resolve`'s behaviour
    /// so a user sees the whole picture in one pass.
    pub fn plan(&self, resolved: &Resolved) -> Result<Vec<Planned>, Vec<EmitError>> {
        let mut planned = Vec::new();
        let mut errors = Vec::new();

        for write in &resolved.writes {
            match self.plan_one(write) {
                Ok(p) => planned.push(p),
                Err(e) => errors.push(e),
            }
        }

        if errors.is_empty() {
            Ok(planned)
        } else {
            Err(errors)
        }
    }

    fn plan_one(&self, write: &Write) -> Result<Planned, EmitError> {
        let path = self.path_for(&write.target);
        let previous = match fs::read_to_string(&path) {
            Ok(s) => Some(s),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };

        let contents = match &write.kind {
            WriteKind::Whole(v) => render(v),
            WriteKind::Projected(fields) => {
                composite(&write.target, fields, previous.as_deref(), &path)?
            }
            // No merge with `previous`: cosmic.conf owns this value outright,
            // which is the whole point of the one-way model. Anything set in
            // COSMIC's own settings UI is replaced, not accumulated.
            WriteKind::Verbatim(s) => s.clone(),
        };

        Ok(Planned {
            path,
            contents,
            previous,
        })
    }

    /// Write the plan. Callers should `plan` first so that failures surface
    /// before any file is touched.
    pub fn apply(&self, planned: &[Planned]) -> Result<usize, EmitError> {
        let mut written = 0;
        for p in planned {
            if p.is_noop() {
                continue;
            }
            if let Some(dir) = p.path.parent() {
                fs::create_dir_all(dir)?;
            }
            atomic_write(&p.path, &p.contents)?;
            written += 1;
        }
        Ok(written)
    }
}

/// Write via temp-file + rename so a reader never observes a partial file.
///
/// The temp name carries cosmic-config's `.atomicwrite` prefix
/// (`cosmic-config/src/lib.rs:408`) so its watcher ignores the intermediate
/// file and reacts only to the final rename.
fn atomic_write(path: &Path, contents: &str) -> io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let tmp = dir.join(format!(".atomicwrite.{name}"));

    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Render a scalar as the RON literal cosmic-config expects.
///
/// Exact formatting is not load-bearing — cosmic-config reads with
/// `ron::from_str` (`lib.rs:468`) — but the *shape* is: `Option<Srgb>` has three
/// components, `Option<Srgba>` four.
fn render(v: &Value) -> String {
    match v {
        Value::Bool(b) => b.to_string(),
        Value::U32(n) => n.to_string(),
        Value::F32(n) => render_f32(*n),
        Value::Str(s) => format!("{s:?}"),
        Value::Rgb(r, g, b) => format!(
            "Some((red: {}, green: {}, blue: {}))",
            render_f32(byte_to_f32(*r)),
            render_f32(byte_to_f32(*g)),
            render_f32(byte_to_f32(*b)),
        ),
        Value::Rgba(r, g, b, a) => format!(
            "Some((red: {}, green: {}, blue: {}, alpha: {}))",
            render_f32(byte_to_f32(*r)),
            render_f32(byte_to_f32(*g)),
            render_f32(byte_to_f32(*b)),
            render_f32(byte_to_f32(*a)),
        ),
    }
}

fn byte_to_f32(b: u8) -> f32 {
    b as f32 / 255.0
}

/// RON needs floats to look like floats: a bare `10` would deserialize as an
/// integer and fail a `f32` field.
fn render_f32(n: f32) -> String {
    if n.fract() == 0.0 {
        format!("{n:.1}")
    } else {
        format!("{n}")
    }
}

/// Rebuild a composite value from folded projections plus whatever is already
/// on disk.
///
/// Only shapes that can be reconstructed correctly are supported. Anything else
/// is a hard error rather than a partial write, because silently writing an
/// incomplete composite would drop the user's other fields.
fn composite(
    target: &TargetKey,
    fields: &BTreeMap<Vec<String>, Value>,
    previous: Option<&str>,
    path: &Path,
) -> Result<String, EmitError> {
    match target.key.as_str() {
        // ThemeBuilder.gaps: (u32, u32) ordered (outer, inner) — theme.rs:895,
        // default (0, 8) — theme.rs:939.
        "gaps" => {
            let (mut outer, mut inner) = match previous {
                Some(text) => {
                    ron::from_str::<(u32, u32)>(text).map_err(|e| EmitError::Unreadable {
                        path: path.to_path_buf(),
                        detail: e.to_string(),
                    })?
                }
                None => (0, 8),
            };

            for (p, v) in fields {
                let Value::U32(n) = v else {
                    return Err(EmitError::UnsupportedComposite {
                        key: target.key.clone(),
                        detail: format!("expected an integer for index {p:?}"),
                    });
                };
                match p.first().map(String::as_str) {
                    Some("0") => outer = *n,
                    Some("1") => inner = *n,
                    other => {
                        return Err(EmitError::UnsupportedComposite {
                            key: target.key.clone(),
                            detail: format!("unknown tuple index {other:?}"),
                        })
                    }
                }
            }
            Ok(format!("({outer}, {inner})"))
        }

        // ThemeBuilder.corner_radii: six [f32; 4] fields — corner.rs:5.
        "corner_radii" => {
            let mut radii = match previous {
                Some(text) => {
                    ron::from_str::<CornerRadii>(text).map_err(|e| EmitError::Unreadable {
                        path: path.to_path_buf(),
                        detail: e.to_string(),
                    })?
                }
                None => CornerRadii::default(),
            };

            for (p, v) in fields {
                let Value::F32(n) = v else {
                    return Err(EmitError::UnsupportedComposite {
                        key: target.key.clone(),
                        detail: format!("expected a number for {p:?}"),
                    });
                };
                let Some(name) = p.first() else {
                    return Err(EmitError::UnsupportedComposite {
                        key: target.key.clone(),
                        detail: "missing radius name".into(),
                    });
                };
                let Some(slot) = radii.field_mut(name) else {
                    return Err(EmitError::UnsupportedComposite {
                        key: target.key.clone(),
                        detail: format!("unknown radius `{name}`"),
                    });
                };
                // A single `rounding` value applies to all four corners.
                *slot = [*n; 4];
            }

            ron::ser::to_string_pretty(&radii, ron::ser::PrettyConfig::new()).map_err(|e| {
                EmitError::UnsupportedComposite {
                    key: target.key.clone(),
                    detail: e.to_string(),
                }
            })
        }

        other => Err(EmitError::UnsupportedComposite {
            key: other.to_string(),
            detail: format!(
                "composite shape not modelled yet; {} field(s) would be written blind",
                fields.len()
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parse, resolve};
    use tempfile::TempDir;

    fn plan_for(src: &str, root: &Path) -> Result<Vec<Planned>, Vec<EmitError>> {
        let ast = parse(src).expect("parse");
        let resolved = resolve(&ast).expect("resolve");
        Emitter::with_root(root).plan(&resolved)
    }

    fn read(root: &Path, component: &str, key: &str) -> String {
        fs::read_to_string(root.join(component).join("v1").join(key))
            .unwrap_or_else(|e| panic!("reading {component}/v1/{key}: {e}"))
    }

    #[test]
    fn writes_land_on_the_cosmic_config_path_layout() {
        let tmp = TempDir::new().unwrap();
        let planned = plan_for("general {\n  autotile = true\n}\n", tmp.path()).unwrap();
        let e = Emitter::with_root(tmp.path());
        e.apply(&planned).unwrap();

        assert_eq!(
            read(tmp.path(), "com.system76.CosmicComp", "autotile"),
            "true"
        );
    }

    #[test]
    fn scalars_render_as_ron_literals() {
        let tmp = TempDir::new().unwrap();
        let src = "general {\n  autotile = true\n  edge_snap_threshold = 12\n}\n\
                   theme {\n  icon_theme = Tela-circle-dracula\n}\n";
        let planned = plan_for(src, tmp.path()).unwrap();
        Emitter::with_root(tmp.path()).apply(&planned).unwrap();

        let root = tmp.path();
        assert_eq!(read(root, "com.system76.CosmicComp", "autotile"), "true");
        assert_eq!(
            read(root, "com.system76.CosmicComp", "edge_snap_threshold"),
            "12"
        );
        assert_eq!(
            read(root, "com.system76.CosmicTk", "icon_theme"),
            "\"Tela-circle-dracula\""
        );
    }

    /// The end-to-end form of the folding property: both halves must reach disk
    /// in one tuple.
    #[test]
    fn both_gaps_reach_disk_in_one_tuple() {
        let tmp = TempDir::new().unwrap();
        let planned =
            plan_for("general {\n  gaps_in = 3\n  gaps_out = 8\n}\n", tmp.path()).unwrap();
        Emitter::with_root(tmp.path()).apply(&planned).unwrap();

        // (outer, inner) — theme.rs:895
        for builder in [
            "com.system76.CosmicTheme.Dark.Builder",
            "com.system76.CosmicTheme.Light.Builder",
        ] {
            assert_eq!(read(tmp.path(), builder, "gaps"), "(8, 3)");
        }
    }

    /// cosmic-config is sparse: an unset key has no file, so a partial
    /// projection must fall back to the verified default rather than zero.
    #[test]
    fn partial_projection_uses_the_verified_default() {
        let tmp = TempDir::new().unwrap();
        let planned = plan_for("general {\n  gaps_in = 5\n}\n", tmp.path()).unwrap();
        Emitter::with_root(tmp.path()).apply(&planned).unwrap();

        // Default is (0, 8); only inner was set, so outer stays 0.
        assert_eq!(
            read(tmp.path(), "com.system76.CosmicTheme.Dark.Builder", "gaps"),
            "(0, 5)"
        );
    }

    /// Read-modify-write must preserve the half the user did not mention.
    #[test]
    fn partial_projection_preserves_existing_sibling() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp
            .path()
            .join("com.system76.CosmicTheme.Dark.Builder")
            .join("v1");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("gaps"), "(20, 4)").unwrap();

        let planned = plan_for("general {\n  gaps_in = 7\n}\n", tmp.path()).unwrap();
        Emitter::with_root(tmp.path()).apply(&planned).unwrap();

        // Outer 20 survives; inner becomes 7.
        assert_eq!(
            read(tmp.path(), "com.system76.CosmicTheme.Dark.Builder", "gaps"),
            "(20, 7)"
        );
    }

    #[test]
    fn colors_render_with_the_right_component_count() {
        let tmp = TempDir::new().unwrap();
        let src = "theme {\n  accent = rgb(ff0000)\n  bg_color = rgba(00ff0080)\n}\n";
        let planned = plan_for(src, tmp.path()).unwrap();
        Emitter::with_root(tmp.path()).apply(&planned).unwrap();

        let b = "com.system76.CosmicTheme.Dark.Builder";
        // Option<Srgb>: three components, no alpha.
        assert_eq!(
            read(tmp.path(), b, "accent"),
            "Some((red: 1.0, green: 0.0, blue: 0.0))"
        );
        // Option<Srgba>: four.
        let bg = read(tmp.path(), b, "bg_color");
        assert!(
            bg.starts_with("Some((red: 0.0, green: 1.0, blue: 0.0, alpha: "),
            "{bg}"
        );
    }

    #[test]
    fn rounding_sets_all_four_corners_of_radius_m() {
        let tmp = TempDir::new().unwrap();
        let planned = plan_for("decoration {\n  rounding = 10\n}\n", tmp.path()).unwrap();
        Emitter::with_root(tmp.path()).apply(&planned).unwrap();

        let text = read(
            tmp.path(),
            "com.system76.CosmicTheme.Dark.Builder",
            "corner_radii",
        );
        let radii: CornerRadii = ron::from_str(&text).expect("round-trips as CornerRadii");
        assert_eq!(radii.radius_m, [10.0; 4]);
    }

    #[test]
    fn rounding_preserves_sibling_radii() {
        // The other five radii must survive a read-modify-write untouched.
        let tmp = TempDir::new().unwrap();
        let planned = plan_for("decoration {\n  rounding = 10\n}\n", tmp.path()).unwrap();
        Emitter::with_root(tmp.path()).apply(&planned).unwrap();

        let text = read(
            tmp.path(),
            "com.system76.CosmicTheme.Dark.Builder",
            "corner_radii",
        );
        let radii: CornerRadii = ron::from_str(&text).unwrap();
        let d = CornerRadii::default();
        assert_eq!(radii.radius_0, d.radius_0);
        assert_eq!(radii.radius_xs, d.radius_xs);
        assert_eq!(radii.radius_s, d.radius_s);
        assert_eq!(radii.radius_l, d.radius_l);
        assert_eq!(radii.radius_xl, d.radius_xl);
    }

    #[test]
    fn corner_radii_defaults_match_upstream() {
        // Pinned against cosmic-theme/src/model/corner.rs:20-31. If upstream
        // changes these, writing a sparse config would silently shift the theme.
        let d = CornerRadii::default();
        assert_eq!(d.radius_0, [0.0; 4]);
        assert_eq!(d.radius_xs, [4.0; 4]);
        assert_eq!(d.radius_s, [8.0; 4]);
        assert_eq!(d.radius_m, [16.0; 4]);
        assert_eq!(d.radius_l, [32.0; 4]);
        assert_eq!(d.radius_xl, [160.0; 4]);
    }

    #[test]
    fn genuinely_unmodelled_composite_is_still_refused() {
        // A projected target with no shape handler must error rather than
        // write a partial value.
        let write = Write {
            target: TargetKey {
                component: "com.system76.Whatever".into(),
                version: 1,
                key: "palette".into(),
            },
            kind: WriteKind::Projected(BTreeMap::from([(
                vec!["bright_red".to_string()],
                Value::U32(1),
            )])),
        };
        let tmp = TempDir::new().unwrap();
        let e = Emitter::with_root(tmp.path());
        let res = e.plan(&Resolved {
            writes: vec![write],
        });
        assert!(matches!(
            res.unwrap_err()[0],
            EmitError::UnsupportedComposite { .. }
        ),);
    }

    #[test]
    fn unparseable_existing_value_is_refused() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp
            .path()
            .join("com.system76.CosmicTheme.Dark.Builder")
            .join("v1");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("gaps"), "not ron at all").unwrap();

        let errs = plan_for("general {\n  gaps_in = 3\n}\n", tmp.path()).unwrap_err();
        assert!(
            matches!(errs[0], EmitError::Unreadable { .. }),
            "{:?}",
            errs[0]
        );
    }

    /// Planning must not touch disk — that is what makes emission transactional.
    #[test]
    fn plan_does_not_write() {
        let tmp = TempDir::new().unwrap();
        let _ = plan_for("general {\n  autotile = true\n}\n", tmp.path()).unwrap();
        assert!(
            fs::read_dir(tmp.path()).unwrap().next().is_none(),
            "plan must leave the tree untouched"
        );
    }

    #[test]
    fn noop_writes_are_skipped() {
        let tmp = TempDir::new().unwrap();
        let src = "general {\n  autotile = true\n}\n";

        let planned = plan_for(src, tmp.path()).unwrap();
        assert_eq!(Emitter::with_root(tmp.path()).apply(&planned).unwrap(), 1);

        // Second run sees identical contents and writes nothing.
        let planned = plan_for(src, tmp.path()).unwrap();
        assert!(planned[0].is_noop());
        assert_eq!(Emitter::with_root(tmp.path()).apply(&planned).unwrap(), 0);
    }

    #[test]
    fn previous_contents_are_captured_for_diffing() {
        let tmp = TempDir::new().unwrap();
        let first = plan_for("general {\n  autotile = true\n}\n", tmp.path()).unwrap();
        Emitter::with_root(tmp.path()).apply(&first).unwrap();

        let second = plan_for("general {\n  autotile = false\n}\n", tmp.path()).unwrap();
        assert_eq!(second[0].previous.as_deref(), Some("true"));
        assert_eq!(second[0].contents, "false");
    }

    #[test]
    fn atomic_write_leaves_no_temp_file() {
        let tmp = TempDir::new().unwrap();
        let planned = plan_for("general {\n  autotile = true\n}\n", tmp.path()).unwrap();
        Emitter::with_root(tmp.path()).apply(&planned).unwrap();

        let dir = tmp.path().join("com.system76.CosmicComp").join("v1");
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with(".atomicwrite"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
    }

    #[test]
    fn from_env_honours_xdg_config_home() {
        // Uses the documented fallback chain rather than a hardcoded path.
        let prev = std::env::var_os("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_HOME", "/tmp/xdg-probe");
        let e = Emitter::from_env().unwrap();
        assert_eq!(e.root(), Path::new("/tmp/xdg-probe/cosmic"));
        match prev {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
    }
}
