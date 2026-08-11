//! AST + schema -> validated, folded writes.
//!
//! Two jobs matter here:
//!
//! 1. **Validation is total before anything is emitted.** A malformed file must
//!    leave the desktop untouched rather than half-applied, so `resolve` returns
//!    every diagnostic it can find and `emit` never sees a partial result.
//! 2. **Projections are folded per target.** Several conf keys can write into
//!    one composite cosmic-config value (`gaps_in` and `gaps_out` are two halves
//!    of one `(u32, u32)`). Writing them independently would let the second
//!    clobber the first, so they are merged into a single write.

use std::collections::BTreeMap;

use crate::bind;
use crate::parser::{Ast, Item, Span, Spanned};
use crate::schema::{self, Entry, Range, Target, Ty};
use crate::windowrule;
use crate::workspace;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    U32(u32),
    F32(f32),
    Str(String),
    /// `Option<Srgb>` target — no alpha channel.
    Rgb(u8, u8, u8),
    /// `Option<Srgba>` target — carries alpha.
    Rgba(u8, u8, u8, u8),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    pub message: String,
    pub span: Span,
    pub help: Option<String>,
}

/// Identifies one cosmic-config value. Ordering is deterministic so emitted
/// writes are stable across runs, which keeps `--diff` output readable.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TargetKey {
    pub component: String,
    pub version: u8,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WriteKind {
    /// The conf key owns the whole value.
    Whole(Value),
    /// Field path -> value, folded from every conf key touching this target.
    Projected(BTreeMap<Vec<String>, Value>),
    /// Pre-rendered RON owning the whole value.
    ///
    /// Used where the target's shape is a collection rather than a scalar, so
    /// there is no `Value` to coerce into: keybindings fold many `bind` lines
    /// into one map. Rendering happens at the point that understands the shape
    /// (`bind::render`) instead of being reconstructed in `emit`.
    Verbatim(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Write {
    pub target: TargetKey,
    pub kind: WriteKind,
}

#[derive(Debug, Default)]
pub struct Resolved {
    pub writes: Vec<Write>,
}

/// Flatten the AST into dotted `section.key` paths, dropping `source` items —
/// include expansion happens before `resolve` so that spans stay attributable
/// to the file they came from.
fn flatten(items: &[Item], prefix: &str, out: &mut Vec<(String, Spanned<String>, Span)>) {
    for item in items {
        match item {
            Item::Section { name, items } => {
                let next = if prefix.is_empty() {
                    name.value.clone()
                } else {
                    format!("{prefix}.{}", name.value)
                };
                flatten(items, &next, out);
            }
            Item::Assign { key, value } => {
                let dotted = if prefix.is_empty() {
                    key.value.clone()
                } else {
                    format!("{prefix}.{}", key.value)
                };
                out.push((dotted, value.clone(), key.span));
            }
            Item::VarDef { .. } | Item::Source { .. } => {}
        }
    }
}

fn collect_vars(items: &[Item], out: &mut BTreeMap<String, String>) {
    for item in items {
        match item {
            Item::VarDef { name, value } => {
                out.insert(name.value.clone(), value.value.clone());
            }
            Item::Section { items, .. } => collect_vars(items, out),
            _ => {}
        }
    }
}

/// Substitute `$name` occurrences. Longest-name-first avoids `$gap` eating the
/// prefix of `$gaps`.
fn expand_vars(input: &str, vars: &BTreeMap<String, String>) -> String {
    if !input.contains('$') {
        return input.to_string();
    }
    let mut names: Vec<&String> = vars.keys().collect();
    names.sort_by_key(|n| std::cmp::Reverse(n.len()));

    let mut out = input.to_string();
    for name in names {
        out = out.replace(&format!("${name}"), &vars[name]);
    }
    out
}

/// Evaluate the tiny arithmetic the format allows: `a * b`, `a + b`, `a - b`.
/// Anything else is returned untouched for the type parser to reject.
fn eval_arith(input: &str) -> String {
    for op in ['*', '+', '-'] {
        if let Some((l, r)) = input.split_once(op) {
            let (l, r) = (l.trim(), r.trim());
            if let (Ok(a), Ok(b)) = (l.parse::<f64>(), r.parse::<f64>()) {
                let v = match op {
                    '*' => a * b,
                    '+' => a + b,
                    _ => a - b,
                };
                return if v.fract() == 0.0 {
                    format!("{}", v as i64)
                } else {
                    format!("{v}")
                };
            }
        }
    }
    input.to_string()
}

/// Parse `rgb(rrggbb)` or `rgba(rrggbbaa)`.
///
/// Bare `#rrggbb` is deliberately **not** accepted: `#` begins a comment, so the
/// value would be stripped before reaching here. Hyprland makes the same
/// trade-off, and HyDE themes write colours as `rgba(...)`, so nothing is lost.
fn parse_color(raw: &str) -> Option<(u8, u8, u8, u8)> {
    let s = raw.trim();
    // Both spellings take the same body; the alpha pair is optional either
    // way, so `rgb(rrggbbaa)` and `rgba(rrggbb)` are accepted too rather than
    // rejected on a technicality.
    let inner = s
        .strip_prefix("rgba(")
        .or_else(|| s.strip_prefix("rgb("))
        .and_then(|s| s.strip_suffix(')'))?;

    let hex = inner.trim().trim_start_matches('#');
    // `len` and the slicing below are both in bytes, so a multi-byte character
    // would make `hex[i..i + 2]` split a char boundary and panic. A config
    // typo must not crash the compiler.
    if !hex.is_ascii() {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    match hex.len() {
        6 => Some((byte(0)?, byte(2)?, byte(4)?, 255)),
        8 => Some((byte(0)?, byte(2)?, byte(4)?, byte(6)?)),
        _ => None,
    }
}

fn coerce(raw: &str, ty: Ty, span: Span) -> Result<Value, Diagnostic> {
    let bad = |expected: &str| Diagnostic {
        message: format!("expected {expected}, found `{raw}`"),
        span,
        help: None,
    };

    match ty {
        Ty::Bool => match raw {
            "true" | "yes" | "on" | "1" => Ok(Value::Bool(true)),
            "false" | "no" | "off" | "0" => Ok(Value::Bool(false)),
            _ => Err(bad("a boolean (true/false/yes/no/on/off)")),
        },
        Ty::U32 => raw
            .parse::<u32>()
            .map(Value::U32)
            .map_err(|_| bad("a non-negative integer")),
        Ty::F32 => raw
            .parse::<f32>()
            .map(Value::F32)
            .map_err(|_| bad("a number")),
        Ty::Str => Ok(Value::Str(raw.to_string())),
        Ty::Rgb => parse_color(raw)
            .map(|(r, g, b, _)| Value::Rgb(r, g, b))
            .ok_or_else(|| bad("a colour like rgb(6b9fed)")),
        Ty::Rgba => parse_color(raw)
            .map(|(r, g, b, a)| Value::Rgba(r, g, b, a))
            .ok_or_else(|| bad("a colour like rgb(6b9fed) or rgba(6b9fed80)")),
        Ty::Mode => match raw {
            "dark" => Ok(Value::Bool(true)),
            "light" => Ok(Value::Bool(false)),
            _ => Err(bad("`dark` or `light`")),
        },
        Ty::FollowMouse => match raw {
            "0" => Ok(Value::Bool(false)),
            "1" => Ok(Value::Bool(true)),
            // Named separately from the catch-all so the message can say why a
            // value that is valid in Hyprland does not work here.
            "2" | "3" => Err(Diagnostic {
                message: format!("`follow_mouse = {raw}` has no COSMIC equivalent"),
                span,
                help: Some(
                    "cosmic-comp has a single focus rather than separate pointer and \
                     keyboard focus, so it cannot detach them. Use `1` for focus follows \
                     mouse or `0` for click to focus."
                        .into(),
                ),
            }),
            _ => Err(bad("`0` (click to focus) or `1` (focus follows mouse)")),
        },
    }
}

fn check_range(v: &Value, range: Option<Range>, span: Span) -> Result<(), Diagnostic> {
    let Some(r) = range else { return Ok(()) };
    let n = match v {
        Value::U32(n) => *n as f64,
        Value::F32(n) => *n as f64,
        _ => return Ok(()),
    };
    if n < r.min || n > r.max {
        return Err(Diagnostic {
            message: format!(
                "value {n} is outside the allowed range {}..={}",
                r.min, r.max
            ),
            span,
            help: None,
        });
    }
    Ok(())
}

/// Resolve an AST against the registry.
///
/// Returns **all** diagnostics rather than the first, so a user fixing a config
/// sees the whole picture in one pass.
pub fn resolve(ast: &Ast) -> Result<Resolved, Vec<Diagnostic>> {
    let mut vars = BTreeMap::new();
    collect_vars(&ast.items, &mut vars);

    let mut flat = Vec::new();
    flatten(&ast.items, "", &mut flat);

    let mut diags = Vec::new();
    // (target) -> folded projections, plus whole-value writes kept separate so
    // a collision between the two can be reported rather than silently resolved.
    let mut projected: BTreeMap<TargetKey, BTreeMap<Vec<String>, Value>> = BTreeMap::new();
    let mut whole: BTreeMap<TargetKey, (Value, Span)> = BTreeMap::new();

    // `bind`, `workspace` and `windowrule` are the repeatable keys in the
    // language: many lines fold into a single value rather than the last one
    // winning, so none of them can go through the schema, which is built around
    // one conf key naming one value.
    let mut binds: Vec<(bind::Bind, Span)> = Vec::new();
    let mut workspaces: Vec<(workspace::WorkspaceDecl, Span)> = Vec::new();
    let mut window_rules: Vec<windowrule::WindowRuleDecl> = Vec::new();

    for (conf, raw_value, key_span) in &flat {
        // `windowrulev2` was Hyprland's name for this syntax before it became
        // the only one; configs in the wild are still full of it.
        if conf == "windowrule" || conf == "windowrulev2" {
            let expanded = expand_vars(&raw_value.value, &vars);
            // Not deduplicated: two rules can differ only in their title and
            // both be wanted, and the compositor takes the first that matches,
            // so the order they were written in is the whole semantics.
            match windowrule::parse_window_rule(&expanded, raw_value.span) {
                Ok(r) => window_rules.push(r),
                Err(e) => diags.push(Diagnostic {
                    message: e.message,
                    span: e.span,
                    help: e.help,
                }),
            }
            continue;
        }

        if conf == "workspace" {
            let expanded = expand_vars(&raw_value.value, &vars);
            match workspace::parse_workspace(&expanded, raw_value.span) {
                Ok(w) => {
                    if let Some((_, prev_span)) =
                        workspaces.iter().find(|(o, _)| o.index == w.index)
                    {
                        diags.push(Diagnostic {
                            message: format!("workspace {} is already declared", w.index),
                            span: raw_value.span,
                            help: Some(format!(
                                "the earlier declaration is on line {}",
                                prev_span.line
                            )),
                        });
                        continue;
                    }
                    workspaces.push((w, raw_value.span));
                }
                Err(e) => diags.push(Diagnostic {
                    message: e.message,
                    span: e.span,
                    help: e.help,
                }),
            }
            continue;
        }

        if conf == "bind" {
            let expanded = expand_vars(&raw_value.value, &vars);
            match bind::parse_bind(&expanded, raw_value.span) {
                Ok(b) => {
                    if let Some((prev, prev_span)) = binds
                        .iter()
                        .find(|(o, _)| o.mods == b.mods && o.key == b.key)
                    {
                        diags.push(Diagnostic {
                            message: format!(
                                "this key combination is already bound to `{}`",
                                prev.action
                            ),
                            span: raw_value.span,
                            help: Some(format!("the earlier bind is on line {}", prev_span.line)),
                        });
                        continue;
                    }
                    binds.push((b, raw_value.span));
                }
                Err(e) => diags.push(Diagnostic {
                    message: e.message,
                    span: e.span,
                    help: e.help,
                }),
            }
            continue;
        }

        let Some(entry) = schema::lookup(conf) else {
            diags.push(Diagnostic {
                message: format!("unknown key `{conf}`"),
                span: *key_span,
                help: schema::suggest(conf).map(|s| format!("did you mean `{s}`?")),
            });
            continue;
        };

        let expanded = eval_arith(&expand_vars(&raw_value.value, &vars));
        let value = match coerce(&expanded, entry.ty, raw_value.span) {
            Ok(v) => v,
            Err(d) => {
                diags.push(d);
                continue;
            }
        };
        if let Err(d) = check_range(&value, entry.validate, raw_value.span) {
            diags.push(d);
            continue;
        }

        record(
            entry,
            value,
            raw_value.span,
            &mut projected,
            &mut whole,
            &mut diags,
        );
    }

    if !diags.is_empty() {
        return Err(diags);
    }

    // A pinned workspace carries its own `tiling_enabled`, so one that did not
    // say has to inherit the session default rather than default to off --
    // otherwise declaring workspaces would quietly undo `general:autotile`.
    // Read before `whole` is consumed below.
    let default_tiling = whole
        .get(&TargetKey {
            component: "com.system76.CosmicComp".into(),
            version: 1,
            key: "autotile".into(),
        })
        .map(|(v, _)| *v == Value::Bool(true))
        .unwrap_or(false);

    let mut writes: Vec<Write> = whole
        .into_iter()
        .map(|(target, (v, _))| Write {
            target,
            kind: WriteKind::Whole(v),
        })
        .collect();

    writes.extend(projected.into_iter().map(|(target, fields)| Write {
        target,
        kind: WriteKind::Projected(fields),
    }));

    if !binds.is_empty() {
        let rendered = bind::render(&binds.iter().map(|(b, _)| b.clone()).collect::<Vec<_>>());
        writes.push(Write {
            // cosmic-comp merges `custom` over `defaults`
            // (cosmic-settings-daemon `config/src/shortcuts/mod.rs`), so writing
            // here overrides a stock shortcut without touching the system file.
            target: TargetKey {
                component: "com.system76.CosmicSettings.Shortcuts".into(),
                version: 1,
                key: "custom".into(),
            },
            kind: WriteKind::Verbatim(rendered),
        });
    }

    if !workspaces.is_empty() {
        let rendered = workspace::render(
            &workspaces.iter().map(|(w, _)| w.clone()).collect::<Vec<_>>(),
            default_tiling,
        );
        writes.push(Write {
            // `Workspaces::add_output` drains this into the first output that
            // appears (cosmic-comp `shell/mod.rs`), and `from_pinned` sets
            // `pinned: true`, which is what stops `can_auto_remove` collecting
            // the workspace the moment its last window closes.
            target: TargetKey {
                component: "com.system76.CosmicComp".into(),
                version: 1,
                key: "pinned_workspaces".into(),
            },
            kind: WriteKind::Verbatim(rendered),
        });
    }

    if !window_rules.is_empty() {
        let rendered = windowrule::render(&window_rules);
        writes.push(Write {
            // Read live: cosmic-comp's config watcher has a `window_rules` arm,
            // so an edit applies to the next window that opens. Unlike
            // `pinned_workspaces`, which waits for the next login.
            target: TargetKey {
                component: "com.system76.CosmicComp".into(),
                version: 1,
                key: "window_rules".into(),
            },
            kind: WriteKind::Verbatim(rendered),
        });
    }

    writes.sort_by(|a, b| a.target.cmp(&b.target));
    Ok(Resolved { writes })
}

fn record(
    entry: &Entry,
    value: Value,
    span: Span,
    projected: &mut BTreeMap<TargetKey, BTreeMap<Vec<String>, Value>>,
    whole: &mut BTreeMap<TargetKey, (Value, Span)>,
    diags: &mut Vec<Diagnostic>,
) {
    for target in entry.targets {
        let tk = TargetKey {
            component: target.component().to_string(),
            version: target.version(),
            key: target.key().to_string(),
        };

        match target {
            Target::Direct { .. } => {
                if projected.contains_key(&tk) {
                    diags.push(Diagnostic {
                        message: format!(
                            "`{}` writes all of `{}`, but another key writes one of its fields",
                            entry.conf, tk.key
                        ),
                        span,
                        help: None,
                    });
                    continue;
                }
                whole.insert(tk, (value.clone(), span));
            }
            Target::Projected { path, .. } => {
                if whole.contains_key(&tk) {
                    diags.push(Diagnostic {
                        message: format!(
                            "`{}` writes a field of `{}`, but another key writes the whole value",
                            entry.conf, tk.key
                        ),
                        span,
                        help: None,
                    });
                    continue;
                }
                let fields = projected.entry(tk).or_default();
                let path: Vec<String> = path.iter().map(|s| s.to_string()).collect();
                fields.insert(path, value.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    fn resolved(src: &str) -> Resolved {
        let ast = parse(src).expect("parse failed");
        resolve(&ast).expect("resolve failed")
    }

    fn errors(src: &str) -> Vec<Diagnostic> {
        let ast = parse(src).expect("parse failed");
        resolve(&ast).unwrap_err()
    }

    #[test]
    fn binds_fold_into_one_write_against_the_shortcuts_custom_key() {
        let r = resolved("bind = SUPER, D, exec, rofi -show drun\nbind = SUPER, Q, killactive\n");
        let w: Vec<_> = r
            .writes
            .iter()
            .filter(|w| w.target.component == "com.system76.CosmicSettings.Shortcuts")
            .collect();
        assert_eq!(w.len(), 1, "every bind belongs to one map");
        assert_eq!(w[0].target.key, "custom");
        assert_eq!(w[0].target.version, 1);

        let WriteKind::Verbatim(ron) = &w[0].kind else {
            panic!("expected verbatim RON, got {:?}", w[0].kind);
        };
        assert!(
            ron.contains(r#"(modifiers: [Super], key: "d"): Spawn("rofi -show drun")"#),
            "{ron}"
        );
        assert!(
            ron.contains(r#"(modifiers: [Super], key: "q"): Close"#),
            "{ron}"
        );
    }

    #[test]
    fn a_bind_expands_variables_like_the_mainmod_idiom_everyone_uses() {
        // Practically every hyprland.conf opens with `$mainMod = SUPER`.
        let r = resolved("$mainMod = SUPER\nbind = $mainMod, D, exec, rofi -show drun\n");
        let WriteKind::Verbatim(ron) = &r.writes.last().unwrap().kind else {
            panic!("expected verbatim RON");
        };
        assert!(ron.contains("modifiers: [Super]"), "{ron}");
    }

    #[test]
    fn arithmetic_is_not_applied_to_a_command() {
        // `eval_arith` would happily rewrite the `-` in a command line.
        let r = resolved("bind = SUPER, V, exec, pactl set-sink-volume @DEFAULT_SINK@ -5%\n");
        let WriteKind::Verbatim(ron) = &r.writes.last().unwrap().kind else {
            panic!("expected verbatim RON");
        };
        assert!(ron.contains("@DEFAULT_SINK@ -5%"), "{ron}");
    }

    #[test]
    fn binding_the_same_combination_twice_is_an_error_not_a_silent_overwrite() {
        let d = errors("bind = SUPER, D, exec, rofi -show drun\nbind = SUPER, D, killactive\n");
        assert_eq!(d.len(), 1);
        assert!(d[0].message.contains("already bound"), "{}", d[0].message);
        assert!(
            d[0].help.as_ref().unwrap().contains("line 1"),
            "{:?}",
            d[0].help
        );
    }

    #[test]
    fn no_binds_means_the_shortcuts_file_is_left_alone() {
        // Writing an empty map would wipe shortcuts set through COSMIC's UI for
        // anyone whose cosmic.conf simply does not mention keybindings.
        let r = resolved("general {\n    gaps_in = 4\n}\n");
        assert!(r
            .writes
            .iter()
            .all(|w| w.target.component != "com.system76.CosmicSettings.Shortcuts"));
    }

    #[test]
    fn a_bad_bind_is_reported_with_the_rest_of_the_file() {
        let d = errors("bind = SUPER, X, frobnicate\ngeneral {\n    gaps_inn = 8\n}\n");
        assert_eq!(d.len(), 2, "resolve reports everything in one pass: {d:?}");
    }

    #[test]
    fn workspaces_fold_into_one_write_against_the_comp_pinned_workspaces_key() {
        let r = resolved("workspace = 1, name:term\nworkspace = 2, name:web\n");
        let w: Vec<_> = r
            .writes
            .iter()
            .filter(|w| w.target.key == "pinned_workspaces")
            .collect();
        assert_eq!(w.len(), 1, "every workspace belongs to one list");
        assert_eq!(w[0].target.component, "com.system76.CosmicComp");
        assert_eq!(w[0].target.version, 1);

        let WriteKind::Verbatim(ron) = &w[0].kind else {
            panic!("expected verbatim RON, got {:?}", w[0].kind);
        };
        assert!(ron.contains(r#"name: Some("term")"#), "{ron}");
        assert!(ron.contains(r#"name: Some("web")"#), "{ron}");
    }

    /// Without this the pinned workspaces would come back floating for a user
    /// whose whole reason for editing the file was `autotile = true`.
    #[test]
    fn a_workspace_that_did_not_say_inherits_general_autotile() {
        let r = resolved("general {\n  autotile = true\n}\nworkspace = 1\n");
        let WriteKind::Verbatim(ron) = find(&r, "com.system76.CosmicComp", "pinned_workspaces")
        else {
            panic!("expected verbatim RON");
        };
        assert!(ron.contains("tiling_enabled: true"), "{ron}");

        let r = resolved("general {\n  autotile = false\n}\nworkspace = 1\n");
        let WriteKind::Verbatim(ron) = find(&r, "com.system76.CosmicComp", "pinned_workspaces")
        else {
            panic!("expected verbatim RON");
        };
        assert!(ron.contains("tiling_enabled: false"), "{ron}");
    }

    #[test]
    fn declaring_the_same_workspace_twice_is_an_error_not_a_silent_overwrite() {
        let d = errors("workspace = 2, name:web\nworkspace = 2, name:mail\n");
        assert_eq!(d.len(), 1);
        assert!(
            d[0].message.contains("already declared"),
            "{}",
            d[0].message
        );
        assert!(
            d[0].help.as_ref().unwrap().contains("line 1"),
            "{:?}",
            d[0].help
        );
    }

    /// Symmetric with `no_binds_means_the_shortcuts_file_is_left_alone`: writing
    /// an empty list would unpin the workspaces of anyone whose cosmic.conf
    /// simply does not mention them.
    #[test]
    fn no_workspaces_means_the_pinned_list_is_left_alone() {
        let r = resolved("general {\n    gaps_in = 4\n}\n");
        assert!(r.writes.iter().all(|w| w.target.key != "pinned_workspaces"));
    }

    #[test]
    fn a_workspace_expands_variables() {
        let r = resolved("$browser = web\nworkspace = 1, name:$browser\n");
        let WriteKind::Verbatim(ron) = find(&r, "com.system76.CosmicComp", "pinned_workspaces")
        else {
            panic!("expected verbatim RON");
        };
        assert!(ron.contains(r#"name: Some("web")"#), "{ron}");
    }

    #[test]
    fn a_bad_workspace_is_reported_with_the_rest_of_the_file() {
        let d = errors("workspace = 0\ngeneral {\n    gaps_inn = 8\n}\n");
        assert_eq!(d.len(), 2, "resolve reports everything in one pass: {d:?}");
    }

    #[test]
    fn window_rules_fold_into_one_write_against_the_comp_window_rules_key() {
        let r = resolved(
            "windowrule = workspace name:web, class:^(vivaldi)$\n\
             windowrule = workspace 1, class:^(kitty)$\n",
        );
        let w: Vec<_> = r
            .writes
            .iter()
            .filter(|w| w.target.key == "window_rules")
            .collect();
        assert_eq!(w.len(), 1, "every rule belongs to one list");
        assert_eq!(w[0].target.component, "com.system76.CosmicComp");
        assert_eq!(w[0].target.version, 1);

        let WriteKind::Verbatim(ron) = &w[0].kind else {
            panic!("expected verbatim RON, got {:?}", w[0].kind);
        };
        assert!(ron.contains(r#"workspace: Name("web")"#), "{ron}");
        assert!(ron.contains("workspace: Index(1)"), "{ron}");
    }

    /// The compositor takes the first rule that matches, so a file that reads
    /// top to bottom has to be emitted top to bottom.
    #[test]
    fn window_rules_keep_the_order_they_were_written_in() {
        let r = resolved(
            "windowrule = workspace 1, class:^(a)$\n\
             windowrule = workspace 2, class:^(b)$\n\
             windowrule = workspace 3, class:^(c)$\n",
        );
        let WriteKind::Verbatim(ron) = find(&r, "com.system76.CosmicComp", "window_rules") else {
            panic!("expected verbatim RON");
        };
        let seen: Vec<&str> = ron
            .lines()
            .filter(|l| l.contains("app_id:"))
            .map(|l| l.trim())
            .collect();
        assert_eq!(seen.len(), 3);
        assert!(seen[0].contains("^(a)$"), "{ron}");
        assert!(seen[1].contains("^(b)$"), "{ron}");
        assert!(seen[2].contains("^(c)$"), "{ron}");
    }

    /// The v2 spelling is what configs in the wild are written with.
    #[test]
    fn windowrulev2_is_the_same_key() {
        let r = resolved("windowrulev2 = workspace 2, class:^(firefox)$\n");
        let WriteKind::Verbatim(ron) = find(&r, "com.system76.CosmicComp", "window_rules") else {
            panic!("expected verbatim RON");
        };
        assert!(ron.contains("workspace: Index(2)"), "{ron}");
    }

    /// Symmetric with the bind and workspace cases: writing an empty list would
    /// be a change for someone whose cosmic.conf never mentions window rules.
    #[test]
    fn no_window_rules_means_the_list_is_left_alone() {
        let r = resolved("general {\n    gaps_in = 4\n}\n");
        assert!(r.writes.iter().all(|w| w.target.key != "window_rules"));
    }

    #[test]
    fn a_window_rule_expands_variables() {
        let r = resolved("$browser = vivaldi\nwindowrule = workspace 2, class:^($browser)$\n");
        let WriteKind::Verbatim(ron) = find(&r, "com.system76.CosmicComp", "window_rules") else {
            panic!("expected verbatim RON");
        };
        assert!(ron.contains("^(vivaldi)$"), "{ron}");
    }

    #[test]
    fn a_bad_window_rule_is_reported_with_the_rest_of_the_file() {
        let d = errors("windowrule = float, class:foo\ngeneral {\n    gaps_inn = 8\n}\n");
        assert_eq!(d.len(), 2, "resolve reports everything in one pass: {d:?}");
    }

    fn find<'a>(r: &'a Resolved, component: &str, key: &str) -> &'a WriteKind {
        &r.writes
            .iter()
            .find(|w| w.target.component == component && w.target.key == key)
            .unwrap_or_else(|| panic!("no write for {component}/{key}"))
            .kind
    }

    /// The spec's highest-value property: two conf keys writing into one
    /// composite value must fold into a single write carrying both fields.
    #[test]
    fn gaps_in_and_gaps_out_fold_into_one_write() {
        let r = resolved("general {\n  gaps_in = 3\n  gaps_out = 8\n}\n");

        let gap_writes: Vec<_> = r.writes.iter().filter(|w| w.target.key == "gaps").collect();
        // One per theme builder — Dark and Light — and no more.
        assert_eq!(gap_writes.len(), 2, "expected one folded write per builder");

        for w in gap_writes {
            match &w.kind {
                WriteKind::Projected(fields) => {
                    assert_eq!(fields.len(), 2, "both halves must survive folding");
                    assert_eq!(fields[&vec!["1".to_string()]], Value::U32(3), "inner");
                    assert_eq!(fields[&vec!["0".to_string()]], Value::U32(8), "outer");
                }
                other => panic!("expected Projected, got {other:?}"),
            }
        }
    }

    #[test]
    fn gaps_land_on_the_verified_tuple_indices() {
        // (outer, inner) per theme.rs:895 — swapping these silently ruins the
        // user's layout, so assert the concrete indices.
        let r = resolved("general {\n  gaps_in = 3\n  gaps_out = 8\n}\n");
        match find(&r, "com.system76.CosmicTheme.Dark.Builder", "gaps") {
            WriteKind::Projected(f) => {
                assert_eq!(f[&vec!["0".to_string()]], Value::U32(8));
                assert_eq!(f[&vec!["1".to_string()]], Value::U32(3));
            }
            other => panic!("expected Projected, got {other:?}"),
        }
    }

    #[test]
    fn direct_keys_produce_whole_writes() {
        let r = resolved("general {\n  autotile = true\n}\n");
        assert_eq!(
            find(&r, "com.system76.CosmicComp", "autotile"),
            &WriteKind::Whole(Value::Bool(true))
        );
    }

    #[test]
    fn variables_expand() {
        let r = resolved("$gap = 5\ngeneral {\n  gaps_in = $gap\n}\n");
        match find(&r, "com.system76.CosmicTheme.Dark.Builder", "gaps") {
            WriteKind::Projected(f) => assert_eq!(f[&vec!["1".to_string()]], Value::U32(5)),
            other => panic!("expected Projected, got {other:?}"),
        }
    }

    #[test]
    fn arithmetic_on_variables_works() {
        let r = resolved("$gap = 4\ngeneral {\n  gaps_out = $gap * 2\n}\n");
        match find(&r, "com.system76.CosmicTheme.Dark.Builder", "gaps") {
            WriteKind::Projected(f) => assert_eq!(f[&vec!["0".to_string()]], Value::U32(8)),
            other => panic!("expected Projected, got {other:?}"),
        }
    }

    #[test]
    fn longer_variable_names_win() {
        // `$gap` must not eat the prefix of `$gaps`.
        let r = resolved("$gap = 1\n$gaps = 7\ngeneral {\n  gaps_in = $gaps\n}\n");
        match find(&r, "com.system76.CosmicTheme.Dark.Builder", "gaps") {
            WriteKind::Projected(f) => assert_eq!(f[&vec!["1".to_string()]], Value::U32(7)),
            other => panic!("expected Projected, got {other:?}"),
        }
    }

    #[test]
    fn rgb_colors_parse_and_drop_alpha() {
        // `accent` is Option<Srgb> — theme.rs:856 — so alpha must not appear.
        let r = resolved("theme {\n  accent = rgb(6b9fed)\n}\n");
        assert_eq!(
            find(&r, "com.system76.CosmicTheme.Dark.Builder", "accent"),
            &WriteKind::Whole(Value::Rgb(0x6b, 0x9f, 0xed))
        );
    }

    #[test]
    fn theme_mode_maps_to_is_dark() {
        let r = resolved("theme {\n  mode = dark\n}\n");
        assert_eq!(
            find(&r, "com.system76.CosmicTheme.Mode", "is_dark"),
            &WriteKind::Whole(Value::Bool(true))
        );
        let r = resolved("theme {\n  mode = light\n}\n");
        assert_eq!(
            find(&r, "com.system76.CosmicTheme.Mode", "is_dark"),
            &WriteKind::Whole(Value::Bool(false))
        );
    }

    #[test]
    fn invalid_theme_mode_is_rejected() {
        let d = errors("theme {\n  mode = purple\n}\n");
        assert!(
            d[0].message.contains("`dark` or `light`"),
            "{}",
            d[0].message
        );
    }

    /// `#` always begins a comment, so a bare hex colour is stripped before it
    /// reaches the value parser. This must fail loudly rather than silently
    /// yield an empty value.
    #[test]
    fn bare_hex_color_is_rejected_because_hash_is_a_comment() {
        let d = errors("theme {\n  accent = #6b9fed\n}\n");
        assert_eq!(d.len(), 1);
        assert!(d[0].message.contains("colour"), "{}", d[0].message);
    }

    #[test]
    fn rgba_keeps_alpha() {
        // `bg_color` is Option<Srgba> — theme.rs:852 — so alpha survives.
        let r = resolved("theme {\n  bg_color = rgba(6b9fed80)\n}\n");
        assert_eq!(
            find(&r, "com.system76.CosmicTheme.Dark.Builder", "bg_color"),
            &WriteKind::Whole(Value::Rgba(0x6b, 0x9f, 0xed, 0x80))
        );
    }

    #[test]
    fn a_multibyte_character_in_a_colour_is_an_error_not_a_panic() {
        // `hex.len()` and the slicing that follows it are both in bytes, so
        // "€abc" is six bytes and would have been sliced mid-character.
        for raw in ["rgb(€abc)", "rgba(ff€€ff00)", "rgb(αβγ)"] {
            let d = errors(&format!("theme {{\n  accent = {raw}\n}}\n"));
            assert_eq!(d.len(), 1, "{raw}");
            assert!(d[0].message.contains("colour"), "{}", d[0].message);
        }
    }

    #[test]
    fn unknown_key_suggests_the_near_miss() {
        let d = errors("general {\n  gaps_inn = 8\n}\n");
        assert_eq!(d.len(), 1);
        assert!(d[0].message.contains("unknown key"), "{}", d[0].message);
        assert_eq!(
            d[0].help.as_deref(),
            Some("did you mean `general.gaps_in`?")
        );
        assert_eq!(d[0].span.line, 2);
    }

    #[test]
    fn type_errors_are_reported_against_the_value() {
        let d = errors("general {\n  gaps_in = purple\n}\n");
        assert_eq!(d.len(), 1);
        assert!(
            d[0].message.contains("non-negative integer"),
            "{}",
            d[0].message
        );
    }

    /// The alias has to land on the same cosmic-config key as COSMIC's own
    /// spelling, or the two would be separate settings that merely look alike.
    #[test]
    fn follow_mouse_is_the_same_write_as_focus_follows_cursor() {
        let hypr = resolved("input {\n  follow_mouse = 1\n}\n");
        let cosmic = resolved("general {\n  focus_follows_cursor = true\n}\n");
        assert_eq!(hypr.writes, cosmic.writes);

        assert_eq!(hypr.writes.len(), 1);
        assert_eq!(hypr.writes[0].target.key, "focus_follows_cursor");
        assert_eq!(hypr.writes[0].kind, WriteKind::Whole(Value::Bool(true)));
    }

    #[test]
    fn follow_mouse_zero_is_click_to_focus() {
        let r = resolved("input {\n  follow_mouse = 0\n}\n");
        assert_eq!(r.writes[0].kind, WriteKind::Whole(Value::Bool(false)));
    }

    /// Hyprland accepts 2 and 3, which detach pointer focus from keyboard
    /// focus. cosmic-comp has one focus and cannot, so the values are refused.
    /// Rounding them to 1 would hand click-to-focus users the opposite of what
    /// they asked for and never say so.
    #[test]
    fn follow_mouse_rejects_the_modes_cosmic_cannot_express() {
        for v in ["2", "3"] {
            let d = errors(&format!("input {{\n  follow_mouse = {v}\n}}\n"));
            assert_eq!(d.len(), 1, "{v}: {d:?}");
            assert!(
                d[0].message.contains("no COSMIC equivalent"),
                "{v}: {}",
                d[0].message
            );
            assert!(
                d[0].help.as_deref().unwrap_or_default().contains("Use `1`"),
                "{v}: help should say what to write instead, got {:?}",
                d[0].help
            );
        }
    }

    /// It is an integer setting in Hyprland, so `true` is not one of its
    /// spellings even though the value it resolves to is a boolean.
    #[test]
    fn follow_mouse_does_not_quietly_accept_boolean_spellings() {
        let d = errors("input {\n  follow_mouse = true\n}\n");
        assert_eq!(d.len(), 1);
        assert!(
            d[0].message.contains("`0`") && d[0].message.contains("`1`"),
            "{}",
            d[0].message
        );
    }

    #[test]
    fn follow_mouse_delay_shares_the_delay_key_and_its_range() {
        let r = resolved("input {\n  follow_mouse_delay = 400\n}\n");
        assert_eq!(r.writes[0].target.key, "focus_follows_cursor_delay");
        assert_eq!(r.writes[0].kind, WriteKind::Whole(Value::U32(400)));

        let d = errors("input {\n  follow_mouse_delay = 9001\n}\n");
        assert!(
            d[0].message.contains("outside the allowed range"),
            "{}",
            d[0].message
        );
    }

    /// Both spellings in one file is not an error: the file's own rule is that
    /// the last assignment wins, and these are two names for one target.
    #[test]
    fn the_last_spelling_in_the_file_wins() {
        let r = resolved(
            "general {\n  focus_follows_cursor = true\n}\ninput {\n  follow_mouse = 0\n}\n",
        );
        assert_eq!(r.writes.len(), 1, "one target, not two: {:?}", r.writes);
        assert_eq!(r.writes[0].kind, WriteKind::Whole(Value::Bool(false)));
    }

    #[test]
    fn out_of_range_values_are_rejected() {
        let d = errors("general {\n  gaps_in = 9999\n}\n");
        assert!(
            d[0].message.contains("outside the allowed range"),
            "{}",
            d[0].message
        );
    }

    #[test]
    fn all_diagnostics_are_reported_not_just_the_first() {
        let d = errors("general {\n  gaps_inn = 8\n  autotile = maybe\n}\n");
        assert_eq!(d.len(), 2, "expected both errors, got {d:?}");
    }

    /// Transactionality: any error means zero writes escape.
    #[test]
    fn a_single_error_produces_no_writes() {
        let ast = parse("general {\n  autotile = true\n  gaps_in = nope\n}\n").unwrap();
        assert!(resolve(&ast).is_err(), "must not partially apply");
    }
}
