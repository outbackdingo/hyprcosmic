//! Compiles a single Hyprland-idiom config file into the cosmic-config tree.
//!
//! The pipeline is `parser -> schema -> resolve -> emit`, and it is transactional:
//! `resolve` validates everything before `emit` writes anything, so a malformed
//! file leaves the desktop untouched rather than half-applied.
//!
//! `parser`, `schema` and `resolve` are pure and depend on nothing
//! COSMIC-specific, which keeps the hard logic testable without a compositor
//! running. Only `emit` binds to cosmic-config, behind the `emit` feature.

pub mod emit;
pub mod parser;
pub mod resolve;
pub mod schema;

pub use emit::{EmitError, Emitter, Planned};
pub use parser::{parse, Ast, ParseError, Span};
pub use resolve::{resolve, Diagnostic, Resolved, Value, Write, WriteKind};

/// Render a diagnostic against source text, cargo-style.
pub fn render_diagnostic(source: &str, span: Span, message: &str, help: Option<&str>) -> String {
    let line = source.lines().nth(span.line.saturating_sub(1)).unwrap_or("");
    let gutter = span.line.to_string().len();
    let pad = " ".repeat(gutter);
    let caret = " ".repeat(span.col.saturating_sub(1)) + &"^".repeat(span.len.max(1));

    let mut out = format!(
        "error: {message}\n\
         {pad}--> cosmic.conf:{}:{}\n\
         {pad} |\n\
         {} | {line}\n\
         {pad} | {caret}",
        span.line, span.col, span.line
    );
    if let Some(help) = help {
        out.push_str(&format!(" {help}"));
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn end_to_end_valid_config_produces_writes() {
        let src = "\
$accent = rgb(6b9fed)
$gap    = 4

general {
    gaps_in     = $gap
    gaps_out    = $gap * 2
    autotile    = true
}

theme {
    accent = $accent
}
";
        let ast = parse(src).expect("parse");
        let r = resolve(&ast).expect("resolve");
        assert!(!r.writes.is_empty());

        // gaps fold per builder; accent fans out to both; autotile is direct.
        let gaps: Vec<_> = r.writes.iter().filter(|w| w.target.key == "gaps").collect();
        assert_eq!(gaps.len(), 2);
        let accent: Vec<_> = r.writes.iter().filter(|w| w.target.key == "accent").collect();
        assert_eq!(accent.len(), 2);
    }

    #[test]
    fn diagnostic_rendering_points_at_the_offending_token() {
        let src = "general {\n    gaps_inn = 8\n}\n";
        let ast = parse(src).unwrap();
        let diags = resolve(&ast).unwrap_err();
        let out = render_diagnostic(src, diags[0].span, &diags[0].message, diags[0].help.as_deref());

        assert!(out.contains("unknown key"), "{out}");
        assert!(out.contains("cosmic.conf:2:5"), "{out}");
        assert!(out.contains("^^^^^^^^"), "{out}");
        assert!(out.contains("did you mean"), "{out}");
    }
}
