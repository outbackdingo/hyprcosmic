//! Declarative registry mapping `cosmic.conf` keys onto cosmic-config targets.
//!
//! This is data, not code: adding a knob is a table row. Every fact encoded here
//! was verified against a checkout rather than assumed — see the spec's
//! "Verified findings" table for file:line evidence.

/// Scalar types a conf value can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ty {
    Bool,
    U32,
    F32,
    Str,
    /// `rgb(rrggbb)` -> `Option<Srgb>` (no alpha). Bare `#rrggbb` is not
    /// accepted: `#` begins a comment.
    Rgb,
    /// `rgb(rrggbb)`/`rgba(rrggbbaa)` -> `Option<Srgba>` (with alpha).
    Rgba,
    /// `dark`/`light` -> the `is_dark` boolean.
    Mode,
}

/// Where a conf key's value lands in the cosmic-config tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The conf key owns the entire cosmic-config value.
    Direct {
        component: &'static str,
        version: u8,
        key: &'static str,
    },
    /// The conf key owns one field within a composite value. Requires
    /// read-modify-write, and multiple conf keys may share one target.
    Projected {
        component: &'static str,
        version: u8,
        key: &'static str,
        path: &'static [&'static str],
    },
}

impl Target {
    pub fn component(&self) -> &'static str {
        match self {
            Target::Direct { component, .. } | Target::Projected { component, .. } => component,
        }
    }

    pub fn version(&self) -> u8 {
        match self {
            Target::Direct { version, .. } | Target::Projected { version, .. } => *version,
        }
    }

    pub fn key(&self) -> &'static str {
        match self {
            Target::Direct { key, .. } | Target::Projected { key, .. } => key,
        }
    }
}

/// Inclusive numeric bounds, checked during `resolve`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Range {
    pub min: f64,
    pub max: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct Entry {
    /// Dotted path as written in the file, e.g. `general.gaps_in`.
    pub conf: &'static str,
    /// One conf key may fan out to several components — Dark and Light theme
    /// builders are separate cosmic-config components holding the same field.
    pub targets: &'static [Target],
    pub ty: Ty,
    pub validate: Option<Range>,
    /// Generates `cosmic.conf.default`, so the reference file cannot drift.
    pub doc: &'static str,
}

const DARK_BUILDER: &str = "com.system76.CosmicTheme.Dark.Builder";
const LIGHT_BUILDER: &str = "com.system76.CosmicTheme.Light.Builder";
const COMP: &str = "com.system76.CosmicComp";
const TK: &str = "com.system76.CosmicTk";
const THEME_MODE: &str = "com.system76.CosmicTheme.Mode";

/// `ThemeBuilder.gaps` is `(u32, u32)` ordered **(outer, inner)** —
/// `cosmic-theme/src/model/theme.rs:895`. Index 0 is the outer gap.
const GAPS_OUTER_IDX: &str = "0";
const GAPS_INNER_IDX: &str = "1";

macro_rules! both_themes {
    ($key:literal, $path:expr) => {
        &[
            Target::Projected {
                component: DARK_BUILDER,
                version: 1,
                key: $key,
                path: $path,
            },
            Target::Projected {
                component: LIGHT_BUILDER,
                version: 1,
                key: $key,
                path: $path,
            },
        ]
    };
}

/// Whole-value fan-out across both theme builders. An empty projection path
/// would be a lie: these fields are `Option<..>` written in full.
macro_rules! both_themes_direct {
    ($key:literal) => {
        &[
            Target::Direct {
                component: DARK_BUILDER,
                version: 1,
                key: $key,
            },
            Target::Direct {
                component: LIGHT_BUILDER,
                version: 1,
                key: $key,
            },
        ]
    };
}

pub const REGISTRY: &[Entry] = &[
    // ---- general ---------------------------------------------------------
    Entry {
        conf: "general.gaps_in",
        targets: both_themes!("gaps", &[GAPS_INNER_IDX]),
        ty: Ty::U32,
        validate: Some(Range {
            min: 0.0,
            max: 128.0,
        }),
        doc: "Gap between adjacent tiled windows, in px",
    },
    Entry {
        conf: "general.gaps_out",
        targets: both_themes!("gaps", &[GAPS_OUTER_IDX]),
        ty: Ty::U32,
        validate: Some(Range {
            min: 0.0,
            max: 256.0,
        }),
        doc: "Gap between tiled windows and the screen edge, in px",
    },
    Entry {
        conf: "general.autotile",
        targets: &[Target::Direct {
            component: COMP,
            version: 1,
            key: "autotile",
        }],
        ty: Ty::Bool,
        validate: None,
        doc: "Automatically tile new windows",
    },
    Entry {
        conf: "general.active_hint",
        targets: &[Target::Direct {
            component: COMP,
            version: 1,
            key: "active_hint",
        }],
        ty: Ty::Bool,
        validate: None,
        doc: "Draw a hint around the focused window",
    },
    Entry {
        conf: "general.focus_follows_cursor",
        targets: &[Target::Direct {
            component: COMP,
            version: 1,
            key: "focus_follows_cursor",
        }],
        ty: Ty::Bool,
        validate: None,
        doc: "Move keyboard focus when the cursor enters a window",
    },
    Entry {
        conf: "general.focus_follows_cursor_delay",
        targets: &[Target::Direct {
            component: COMP,
            version: 1,
            key: "focus_follows_cursor_delay",
        }],
        ty: Ty::U32,
        validate: Some(Range {
            min: 0.0,
            max: 5000.0,
        }),
        doc: "Delay in ms before focus follows the cursor",
    },
    Entry {
        conf: "general.cursor_follows_focus",
        targets: &[Target::Direct {
            component: COMP,
            version: 1,
            key: "cursor_follows_focus",
        }],
        ty: Ty::Bool,
        validate: None,
        doc: "Warp the cursor to the window that gains keyboard focus",
    },
    Entry {
        conf: "general.edge_snap_threshold",
        targets: &[Target::Direct {
            component: COMP,
            version: 1,
            key: "edge_snap_threshold",
        }],
        ty: Ty::U32,
        validate: Some(Range {
            min: 0.0,
            max: 256.0,
        }),
        doc: "Distance in px at which windows snap to output edges",
    },
    // ---- decoration ------------------------------------------------------
    Entry {
        conf: "decoration.rounding",
        targets: both_themes!("corner_radii", &["radius_m"]),
        ty: Ty::F32,
        validate: Some(Range {
            min: 0.0,
            max: 64.0,
        }),
        doc: "Window corner radius in px (maps to the theme's radius_m)",
    },
    // ---- theme -----------------------------------------------------------
    Entry {
        conf: "theme.mode",
        targets: &[Target::Direct {
            component: THEME_MODE,
            version: 1,
            key: "is_dark",
        }],
        ty: Ty::Mode,
        validate: None,
        doc: "`dark` or `light`",
    },
    Entry {
        conf: "theme.accent",
        targets: both_themes_direct!("accent"),
        ty: Ty::Rgb,
        validate: None,
        doc: "Accent colour as rgb(rrggbb) or rgba(rrggbbaa)",
    },
    Entry {
        conf: "theme.bg_color",
        targets: both_themes_direct!("bg_color"),
        ty: Ty::Rgba,
        validate: None,
        doc: "Background base colour",
    },
    Entry {
        conf: "theme.icon_theme",
        targets: &[Target::Direct {
            component: TK,
            version: 1,
            key: "icon_theme",
        }],
        ty: Ty::Str,
        validate: None,
        doc: "Icon theme name, e.g. Tela-circle-dracula",
    },
];

/// Exact lookup by dotted conf path.
pub fn lookup(conf: &str) -> Option<&'static Entry> {
    REGISTRY.iter().find(|e| e.conf == conf)
}

/// Nearest known key by edit distance, for "did you mean" diagnostics.
/// Only suggests when the candidate is close enough to be plausible.
pub fn suggest(conf: &str) -> Option<&'static str> {
    let budget = match conf.len() {
        0..=4 => 1,
        5..=8 => 2,
        _ => 3,
    };
    REGISTRY
        .iter()
        .map(|e| (edit_distance(conf, e.conf), e.conf))
        .filter(|(d, _)| *d <= budget)
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c)
}

/// Levenshtein distance, two-row variant.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }

    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];

    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gaps_use_the_verified_tuple_order() {
        // ThemeBuilder.gaps is (outer, inner) — theme.rs:895. Getting this
        // backwards silently swaps the user's gaps, so pin it.
        let inner = lookup("general.gaps_in").unwrap();
        let outer = lookup("general.gaps_out").unwrap();

        for t in inner.targets {
            match t {
                Target::Projected { path, .. } => assert_eq!(*path, &["1"]),
                other => panic!("gaps_in should project, got {other:?}"),
            }
        }
        for t in outer.targets {
            match t {
                Target::Projected { path, .. } => assert_eq!(*path, &["0"]),
                other => panic!("gaps_out should project, got {other:?}"),
            }
        }
    }

    #[test]
    fn theme_keys_fan_out_to_dark_and_light() {
        let e = lookup("general.gaps_in").unwrap();
        let comps: Vec<_> = e.targets.iter().map(|t| t.component()).collect();
        assert!(comps.contains(&"com.system76.CosmicTheme.Dark.Builder"));
        assert!(comps.contains(&"com.system76.CosmicTheme.Light.Builder"));
        assert_eq!(comps.len(), 2);
    }

    #[test]
    fn comp_keys_do_not_fan_out() {
        let e = lookup("general.autotile").unwrap();
        assert_eq!(e.targets.len(), 1);
        assert_eq!(e.targets[0].component(), "com.system76.CosmicComp");
    }

    #[test]
    fn every_entry_has_at_least_one_target() {
        for e in REGISTRY {
            assert!(!e.targets.is_empty(), "`{}` has no targets", e.conf);
        }
    }

    #[test]
    fn conf_paths_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for e in REGISTRY {
            assert!(seen.insert(e.conf), "duplicate registry entry `{}`", e.conf);
        }
    }

    #[test]
    fn every_entry_is_documented() {
        // `doc` generates cosmic.conf.default; an empty one would ship a blank
        // reference line.
        for e in REGISTRY {
            assert!(!e.doc.trim().is_empty(), "`{}` has no doc", e.conf);
        }
    }

    #[test]
    fn suggests_near_misses() {
        assert_eq!(suggest("general.gaps_inn"), Some("general.gaps_in"));
        assert_eq!(suggest("general.autotil"), Some("general.autotile"));
    }

    #[test]
    fn does_not_suggest_nonsense() {
        assert_eq!(suggest("completely.unrelated.nonsense.key"), None);
    }
}
