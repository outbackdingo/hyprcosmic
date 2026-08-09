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

use crate::parser::{Ast, Item, Span, Spanned};
use crate::schema::{self, Entry, Range, Target, Ty};

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    U32(u32),
    F32(f32),
    Str(String),
    /// Straight RGBA bytes; conversion to COSMIC's f32 colour struct happens in `emit`.
    Color(u8, u8, u8, u8),
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
fn parse_color(raw: &str) -> Option<Value> {
    let s = raw.trim();
    let hex = if let Some(inner) = s.strip_prefix("rgba(").and_then(|s| s.strip_suffix(')')) {
        inner.trim().to_string()
    } else if let Some(inner) = s.strip_prefix("rgb(").and_then(|s| s.strip_suffix(')')) {
        inner.trim().to_string()
    } else {
        return None;
    };

    let hex = hex.trim_start_matches('#');
    match hex.len() {
        6 => Some(Value::Color(
            u8::from_str_radix(&hex[0..2], 16).ok()?,
            u8::from_str_radix(&hex[2..4], 16).ok()?,
            u8::from_str_radix(&hex[4..6], 16).ok()?,
            255,
        )),
        8 => Some(Value::Color(
            u8::from_str_radix(&hex[0..2], 16).ok()?,
            u8::from_str_radix(&hex[2..4], 16).ok()?,
            u8::from_str_radix(&hex[4..6], 16).ok()?,
            u8::from_str_radix(&hex[6..8], 16).ok()?,
        )),
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
        Ty::Color => parse_color(raw).ok_or_else(|| bad("a colour like rgb(6b9fed) or rgba(6b9fed80)")),
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
            message: format!("value {n} is outside the allowed range {}..={}", r.min, r.max),
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

    for (conf, raw_value, key_span) in &flat {
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

        record(entry, value, raw_value.span, &mut projected, &mut whole, &mut diags);
    }

    if !diags.is_empty() {
        return Err(diags);
    }

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
    fn rgb_colors_parse() {
        let r = resolved("theme {\n  accent = rgb(6b9fed)\n}\n");
        match find(&r, "com.system76.CosmicTheme.Dark.Builder", "accent") {
            WriteKind::Projected(f) => {
                assert_eq!(f[&Vec::<String>::new()], Value::Color(0x6b, 0x9f, 0xed, 255));
            }
            other => panic!("expected Projected, got {other:?}"),
        }
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
        let r = resolved("theme {\n  accent = rgba(6b9fed80)\n}\n");
        match find(&r, "com.system76.CosmicTheme.Dark.Builder", "accent") {
            WriteKind::Projected(f) => {
                assert_eq!(f[&Vec::<String>::new()], Value::Color(0x6b, 0x9f, 0xed, 0x80));
            }
            other => panic!("expected Projected, got {other:?}"),
        }
    }

    #[test]
    fn unknown_key_suggests_the_near_miss() {
        let d = errors("general {\n  gaps_inn = 8\n}\n");
        assert_eq!(d.len(), 1);
        assert!(d[0].message.contains("unknown key"), "{}", d[0].message);
        assert_eq!(d[0].help.as_deref(), Some("did you mean `general.gaps_in`?"));
        assert_eq!(d[0].span.line, 2);
    }

    #[test]
    fn type_errors_are_reported_against_the_value() {
        let d = errors("general {\n  gaps_in = purple\n}\n");
        assert_eq!(d.len(), 1);
        assert!(d[0].message.contains("non-negative integer"), "{}", d[0].message);
    }

    #[test]
    fn out_of_range_values_are_rejected() {
        let d = errors("general {\n  gaps_in = 9999\n}\n");
        assert!(d[0].message.contains("outside the allowed range"), "{}", d[0].message);
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
