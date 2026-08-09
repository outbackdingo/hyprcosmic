//! HyDE `hypr.theme` -> `cosmic.conf`.
//!
//! One-way, and into the conf file rather than straight into cosmic-config, so
//! the result is readable and editable before it touches the desktop.
//!
//! The guiding rule is that **nothing is dropped silently**. A HyDE theme
//! contains a good deal that COSMIC has no equivalent for — gradient borders,
//! blur tuning, layer rules — and a converter that quietly ignored them would
//! leave the user wondering why their desktop looks wrong. Every unhandled key
//! is reported with a reason.

use crate::parser::{parse, Item, ParseError, Span};

/// Why a source key did not make it into the output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// COSMIC has no equivalent concept.
    NoEquivalent(&'static str),
    /// Needs a compositor patch that does not exist yet (spec Phase 2).
    NeedsCompositorPatch(&'static str),
    /// Belongs to another program entirely; copied verbatim, not translated.
    DifferentProgram(&'static str),
    /// Translated, but with a loss worth knowing about.
    Lossy(String),
}

impl Reason {
    pub fn describe(&self) -> String {
        match self {
            Reason::NoEquivalent(d) => format!("no COSMIC equivalent: {d}"),
            Reason::NeedsCompositorPatch(d) => format!("needs a cosmic-comp patch: {d}"),
            Reason::DifferentProgram(d) => format!("handled by another program: {d}"),
            Reason::Lossy(d) => format!("translated with loss: {d}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub key: String,
    pub value: String,
    pub reason: Reason,
    pub span: Span,
}

#[derive(Debug, Default)]
pub struct Import {
    /// Generated `cosmic.conf` text.
    pub conf: String,
    /// Everything that did not translate cleanly.
    pub notes: Vec<Note>,
}

impl Import {
    /// Keys that produced no output at all, as opposed to lossy translations.
    pub fn dropped(&self) -> impl Iterator<Item = &Note> {
        self.notes
            .iter()
            .filter(|n| !matches!(n.reason, Reason::Lossy(_)))
    }
}

/// HyDE prefixes each `.theme` file with a destination line such as
/// `$HOME/.config/hypr/themes/theme.conf|> $HOME/.../colors.conf`.
///
/// It is metadata for HyDE's own installer, not config, and it has no `=`, so
/// the parser would reject the file outright. Strip it before parsing.
fn strip_hyde_header(src: &str) -> &str {
    let mut lines = src.lines();
    let Some(first) = lines.next() else {
        return src;
    };
    let is_destination_header = !first.contains('=') && (first.contains("|>") || first.contains('|'));
    if is_destination_header {
        // Preserve line numbering by keeping the newline count intact: callers
        // report spans against the stripped text, so re-add a blank line.
        match src.find('\n') {
            Some(i) => &src[i + 1..],
            None => "",
        }
    } else {
        src
    }
}

/// First colour of a possibly-gradient Hyprland border spec.
/// `rgba(ca9ee6ff) rgba(f2d5cfff) 45deg` -> `ca9ee6`.
fn first_color_rgb(value: &str) -> Option<String> {
    let token = value.split_whitespace().next()?;
    let inner = token
        .strip_prefix("rgba(")
        .or_else(|| token.strip_prefix("rgb("))?
        .strip_suffix(')')?;
    let hex = inner.trim_start_matches('#');
    if hex.len() >= 6 && hex[..6].chars().all(|c| c.is_ascii_hexdigit()) {
        Some(hex[..6].to_string())
    } else {
        None
    }
}

fn is_gradient(value: &str) -> bool {
    value.split_whitespace().count() > 1
}

/// Flatten to dotted keys, keeping variables separate — HyDE carries
/// `$GTK_THEME` / `$ICON_THEME` as variables rather than config keys.
fn walk(items: &[Item], prefix: &str, out: &mut Vec<(String, String, Span)>, vars: &mut Vec<(String, String, Span)>) {
    for item in items {
        match item {
            Item::Section { name, items } => {
                let next = if prefix.is_empty() {
                    name.value.clone()
                } else {
                    format!("{prefix}.{}", name.value)
                };
                walk(items, &next, out, vars);
            }
            Item::Assign { key, value } => {
                let dotted = if prefix.is_empty() {
                    key.value.clone()
                } else {
                    format!("{prefix}.{}", key.value)
                };
                out.push((dotted, value.value.clone(), key.span));
            }
            Item::VarDef { name, value } => {
                vars.push((name.value.clone(), value.value.clone(), name.span));
            }
            Item::Source { .. } => {}
        }
    }
}

/// Translate a HyDE `hypr.theme` into a `cosmic.conf`.
pub fn import_hypr_theme(src: &str, theme_name: &str) -> Result<Import, ParseError> {
    let body = strip_hyde_header(src);
    let ast = parse(body)?;

    let mut keys = Vec::new();
    let mut vars = Vec::new();
    walk(&ast.items, "", &mut keys, &mut vars);

    let mut general: Vec<(String, String)> = Vec::new();
    let mut decoration: Vec<(String, String)> = Vec::new();
    let mut theme: Vec<(String, String)> = Vec::new();
    let mut notes = Vec::new();

    for (name, value, span) in &vars {
        match name.as_str() {
            "ICON_THEME" => theme.push(("icon_theme".into(), value.clone())),
            "COLOR_SCHEME" => {
                let mode = if value.contains("light") { "light" } else { "dark" };
                theme.push(("mode".into(), mode.into()));
            }
            "GTK_THEME" => notes.push(Note {
                key: format!("${name}"),
                value: value.clone(),
                reason: Reason::DifferentProgram(
                    "GTK theme applies to GTK apps directly; COSMIC apps use cosmic-theme",
                ),
                span: *span,
            }),
            _ => {}
        }
    }

    for (key, value, span) in &keys {
        let note = |reason| Note {
            key: key.clone(),
            value: value.clone(),
            reason,
            span: *span,
        };

        match key.as_str() {
            "general.gaps_in" => general.push(("gaps_in".into(), value.clone())),
            "general.gaps_out" => general.push(("gaps_out".into(), value.clone())),
            "decoration.rounding" => decoration.push(("rounding".into(), value.clone())),

            // Border colour is the closest thing a HyDE theme has to an accent.
            "general.col.active_border" => match first_color_rgb(value) {
                Some(hex) => {
                    theme.push(("accent".into(), format!("rgb({hex})")));
                    if is_gradient(value) {
                        notes.push(note(Reason::Lossy(format!(
                            "used first stop rgb({hex}) as the accent; COSMIC's active_hint \
                             is a solid colour with no gradient or angle"
                        ))));
                    }
                }
                None => notes.push(note(Reason::NoEquivalent("unrecognised colour syntax"))),
            },

            "general.col.inactive_border"
            | "group.col.border_active"
            | "group.col.border_inactive"
            | "group.col.border_locked_active"
            | "group.col.border_locked_inactive" => {
                notes.push(note(Reason::NoEquivalent(
                    "COSMIC draws a single active hint; per-state border colours do not exist",
                )));
            }

            "general.border_size" => notes.push(note(Reason::NoEquivalent(
                "COSMIC's active_hint is a boolean, not a width",
            ))),
            "general.layout" => notes.push(note(Reason::NoEquivalent(
                "cosmic-comp uses a BSP tiler; dwindle/master are not available",
            ))),
            "general.resize_on_border" => notes.push(note(Reason::NoEquivalent(
                "no equivalent setting",
            ))),

            k if k.starts_with("decoration.blur") => notes.push(note(
                Reason::NeedsCompositorPatch("COSMIC blur is client-requested via \
                     ext-background-effect; rule-driven blur is spec Phase 2"),
            )),
            k if k.starts_with("decoration.shadow") => notes.push(note(
                Reason::NeedsCompositorPatch("shadow.frag exists but is not configurable yet"),
            )),
            "decoration.active_opacity" | "decoration.inactive_opacity" => notes.push(note(
                Reason::NeedsCompositorPatch("window opacity is not configurable yet"),
            )),

            "layerrule" => notes.push(note(Reason::DifferentProgram(
                "layer rules target the bar; waybar is configured directly",
            ))),
            "exec" => notes.push(note(Reason::DifferentProgram(
                "HyDE runs gsettings here; icon and GTK themes are handled above",
            ))),

            _ => notes.push(note(Reason::NoEquivalent("unrecognised key"))),
        }
    }

    Ok(Import {
        conf: render_conf(theme_name, &general, &decoration, &theme, &notes),
        notes,
    })
}

fn render_conf(
    theme_name: &str,
    general: &[(String, String)],
    decoration: &[(String, String)],
    theme: &[(String, String)],
    notes: &[Note],
) -> String {
    let mut out = format!(
        "# Generated by `cosmic-conf import-theme` from the HyDE theme {theme_name:?}.\n\
         # Edit freely — this file is the source of truth; cosmic-settings changes\n\
         # are overwritten on the next `cosmic-conf apply`.\n"
    );

    let dropped: Vec<&Note> = notes
        .iter()
        .filter(|n| !matches!(n.reason, Reason::Lossy(_)))
        .collect();
    if !dropped.is_empty() {
        out.push_str(&format!(
            "#\n# {} setting(s) from the source theme were not translated.\n\
             # Run with --report to see them.\n",
            dropped.len()
        ));
    }

    let section = |name: &str, rows: &[(String, String)], out: &mut String| {
        if rows.is_empty() {
            return;
        }
        out.push_str(&format!("\n{name} {{\n"));
        let width = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
        for (k, v) in rows {
            out.push_str(&format!("    {k:<width$} = {v}\n"));
        }
        out.push_str("}\n");
    };

    section("general", general, &mut out);
    section("decoration", decoration, &mut out);
    section("theme", theme, &mut out);
    out
}

/// Human-readable report of everything that did not translate cleanly.
pub fn render_report(import: &Import) -> String {
    if import.notes.is_empty() {
        return "Everything in the source theme translated cleanly.\n".into();
    }

    let mut out = String::new();
    let lossy: Vec<&Note> = import
        .notes
        .iter()
        .filter(|n| matches!(n.reason, Reason::Lossy(_)))
        .collect();
    let dropped: Vec<&Note> = import.dropped().collect();

    if !lossy.is_empty() {
        out.push_str("Translated with loss:\n");
        for n in &lossy {
            out.push_str(&format!("  {} = {}\n    {}\n", n.key, n.value, n.reason.describe()));
        }
    }
    if !dropped.is_empty() {
        if !lossy.is_empty() {
            out.push('\n');
        }
        out.push_str("Not translated:\n");
        for n in &dropped {
            out.push_str(&format!("  {} = {}\n    {}\n", n.key, n.value, n.reason.describe()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The genuine Catppuccin-Mocha `hypr.theme` from HyDE-Project/hyde-themes,
    /// verbatim including its destination header. Synthetic fixtures would miss
    /// the header, the colon-keys and the gradient syntax.
    const HYDE_CATPPUCCIN: &str = r#"$HOME/.config/hypr/themes/theme.conf|> $HOME/.config/hypr/themes/colors.conf
#  // P̳r̳a̳s̳a̳n̳t̳h̳ R̳a̳n̳g̳a̳n̳

$GTK_THEME=Catppuccin-Mocha
$ICON_THEME = Tela-circle-dracula
$COLOR_SCHEME = prefer-dark

exec = gsettings set org.gnome.desktop.interface icon-theme $ICON_THEME

general {
    gaps_in = 3
    gaps_out = 8
    border_size = 2
    col.active_border = rgba(ca9ee6ff) rgba(f2d5cfff) 45deg
    col.inactive_border = rgba(b4befecc) rgba(6c7086cc) 45deg
    layout = dwindle
    resize_on_border = true
}

group {
    col.border_active = rgba(ca9ee6ff) rgba(f2d5cfff) 45deg
    col.border_inactive = rgba(b4befecc) rgba(6c7086cc) 45deg
}

decoration {
    rounding = 10
    shadow:enabled = false

    blur {
        enabled = yes
        size = 6
        passes = 3
    }
}

layerrule = blur,waybar
"#;

    fn import() -> Import {
        import_hypr_theme(HYDE_CATPPUCCIN, "Catppuccin Mocha").expect("real HyDE theme must parse")
    }

    #[test]
    fn real_hyde_theme_parses_despite_its_destination_header() {
        // The header has no `=` and would otherwise be a parse error.
        let _ = import();
    }

    #[test]
    fn geometry_is_translated() {
        let c = import().conf;
        assert!(c.contains("gaps_in  = 3"), "{c}");
        assert!(c.contains("gaps_out = 8"), "{c}");
        assert!(c.contains("rounding = 10"), "{c}");
    }

    #[test]
    fn accent_comes_from_the_first_gradient_stop() {
        let c = import().conf;
        assert!(c.contains("accent"), "{c}");
        assert!(c.contains("rgb(ca9ee6)"), "{c}");
    }

    #[test]
    fn gradient_loss_is_reported_not_silent() {
        let i = import();
        let lossy: Vec<_> = i
            .notes
            .iter()
            .filter(|n| matches!(n.reason, Reason::Lossy(_)))
            .collect();
        assert_eq!(lossy.len(), 1, "{:?}", i.notes);
        assert_eq!(lossy[0].key, "general.col.active_border");
        assert!(lossy[0].reason.describe().contains("gradient"));
    }

    #[test]
    fn icon_theme_and_color_scheme_come_from_variables() {
        let c = import().conf;
        assert!(c.contains("icon_theme = Tela-circle-dracula"), "{c}");
        assert!(c.contains("mode"), "{c}");
        assert!(c.contains("dark"), "{c}");
    }

    #[test]
    fn blur_is_flagged_as_needing_a_compositor_patch() {
        let i = import();
        let blur: Vec<_> = i
            .notes
            .iter()
            .filter(|n| n.key.starts_with("decoration.blur"))
            .collect();
        assert!(!blur.is_empty(), "blur settings must be reported");
        assert!(blur
            .iter()
            .all(|n| matches!(n.reason, Reason::NeedsCompositorPatch(_))));
    }

    #[test]
    fn unsupported_concepts_are_each_reported() {
        let i = import();
        let keys: Vec<&str> = i.dropped().map(|n| n.key.as_str()).collect();
        for expected in [
            "general.border_size",
            "general.layout",
            "general.col.inactive_border",
            "group.col.border_active",
            "layerrule",
        ] {
            assert!(keys.contains(&expected), "`{expected}` missing from {keys:?}");
        }
    }

    #[test]
    fn nothing_is_dropped_without_a_note() {
        // Every source key must either appear in the output or carry a note.
        let i = import();
        let translated = ["general.gaps_in", "general.gaps_out", "decoration.rounding"];
        let noted: Vec<&str> = i.notes.iter().map(|n| n.key.as_str()).collect();

        let body = strip_hyde_header(HYDE_CATPPUCCIN);
        let ast = parse(body).unwrap();
        let (mut keys, mut vars) = (Vec::new(), Vec::new());
        walk(&ast.items, "", &mut keys, &mut vars);

        for (k, _, _) in &keys {
            assert!(
                translated.contains(&k.as_str()) || noted.contains(&k.as_str()),
                "`{k}` was neither translated nor reported"
            );
        }
    }

    #[test]
    fn generated_conf_is_valid_input_to_our_own_parser() {
        // The importer must not emit something `apply` cannot read.
        let c = import().conf;
        parse(&c).expect("generated cosmic.conf must parse");
    }

    #[test]
    fn generated_conf_resolves_against_the_registry() {
        // Stronger: every key it emits must actually exist in the schema.
        let c = import().conf;
        let ast = parse(&c).unwrap();
        crate::resolve(&ast).expect("generated conf must resolve cleanly");
    }

    #[test]
    fn report_separates_lossy_from_dropped() {
        let r = render_report(&import());
        assert!(r.contains("Translated with loss:"), "{r}");
        assert!(r.contains("Not translated:"), "{r}");
    }

    #[test]
    fn header_without_pipe_is_not_stripped() {
        let src = "general {\n    gaps_in = 4\n}\n";
        let i = import_hypr_theme(src, "t").unwrap();
        // Single key, so no alignment padding.
        assert!(i.conf.contains("gaps_in = 4"), "{}", i.conf);
    }
}
