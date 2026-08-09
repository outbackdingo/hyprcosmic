//! `cosmic.conf` text -> AST.
//!
//! Hyprland-idiom, line-based grammar:
//!
//! ```text
//! # comment
//! $var = value
//! section {
//!     key = value
//!     nested { key = value }
//! }
//! bind = SUPER, Q, close      # repeatable keys are kept in order
//! source = ~/other.conf
//! ```
//!
//! Values are kept as raw strings here; typing happens in `resolve`, which needs
//! the schema to know what a value should be.

use std::fmt;

/// Byte-independent source position. Line and column are 1-based so they match
/// what an editor shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub line: usize,
    pub col: usize,
    pub len: usize,
}

impl Span {
    pub fn new(line: usize, col: usize, len: usize) -> Self {
        Self { line, col, len }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spanned<T> {
    pub value: T,
    pub span: Span,
}

impl<T> Spanned<T> {
    pub fn new(value: T, span: Span) -> Self {
        Self { value, span }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// `$name = value`
    VarDef {
        name: Spanned<String>,
        value: Spanned<String>,
    },
    /// `key = value` inside the current section
    Assign {
        key: Spanned<String>,
        value: Spanned<String>,
    },
    /// `name { .. }`
    Section {
        name: Spanned<String>,
        items: Vec<Item>,
    },
    /// `source = path`
    Source { path: Spanned<String> },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ast {
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    pub span: Span,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.span.line, self.span.col, self.message)
    }
}

impl std::error::Error for ParseError {}

/// Strip a trailing `#` comment, respecting nothing else — the grammar has no
/// string literals, so there is no quoting to honour.
fn strip_comment(line: &str) -> &str {
    match line.find('#') {
        Some(i) => &line[..i],
        None => line,
    }
}

/// Column (1-based) of the first non-whitespace byte.
fn indent_col(line: &str) -> usize {
    line.len() - line.trim_start().len() + 1
}

pub fn parse(input: &str) -> Result<Ast, ParseError> {
    let mut cursor = Cursor {
        lines: input.lines().collect(),
        idx: 0,
    };
    let items = parse_items(&mut cursor, 0)?;
    Ok(Ast { items })
}

struct Cursor<'a> {
    lines: Vec<&'a str>,
    idx: usize,
}

/// Parse items until EOF (`depth == 0`) or a closing brace.
fn parse_items(cur: &mut Cursor, depth: usize) -> Result<Vec<Item>, ParseError> {
    let mut items = Vec::new();

    while cur.idx < cur.lines.len() {
        let raw = cur.lines[cur.idx];
        let line_no = cur.idx + 1;
        let content = strip_comment(raw).trim_end();
        let trimmed = content.trim();

        if trimmed.is_empty() {
            cur.idx += 1;
            continue;
        }

        if trimmed == "}" {
            if depth == 0 {
                return Err(ParseError {
                    message: "unmatched `}`".into(),
                    span: Span::new(line_no, indent_col(content), 1),
                });
            }
            cur.idx += 1;
            return Ok(items);
        }

        // `name {` opens a section. A one-line `name { .. }` is not supported;
        // keeping the grammar strictly line-based keeps spans honest.
        if let Some(name) = trimmed.strip_suffix('{') {
            let name = name.trim();
            if name.is_empty() {
                return Err(ParseError {
                    message: "section is missing a name".into(),
                    span: Span::new(line_no, indent_col(content), 1),
                });
            }
            let span = Span::new(line_no, indent_col(content), name.len());
            cur.idx += 1;
            let inner = parse_items(cur, depth + 1)?;
            items.push(Item::Section {
                name: Spanned::new(name.to_string(), span),
                items: inner,
            });
            continue;
        }

        let Some(eq) = content.find('=') else {
            return Err(ParseError {
                message: format!("expected `key = value`, found `{trimmed}`"),
                span: Span::new(line_no, indent_col(content), trimmed.len()),
            });
        };

        let key_raw = &content[..eq];
        let val_raw = &content[eq + 1..];
        let key = key_raw.trim();
        let value = val_raw.trim();

        if key.is_empty() {
            return Err(ParseError {
                message: "assignment is missing a key".into(),
                span: Span::new(line_no, 1, eq.max(1)),
            });
        }

        let key_col = indent_col(content);
        let key_span = Span::new(line_no, key_col, key.len());
        // Column of the value = everything before it, plus its own leading trim.
        let val_col = eq + 2 + (val_raw.len() - val_raw.trim_start().len());
        let val_span = Span::new(line_no, val_col, value.len());

        let item = if let Some(var) = key.strip_prefix('$') {
            if var.is_empty() {
                return Err(ParseError {
                    message: "variable is missing a name after `$`".into(),
                    span: key_span,
                });
            }
            Item::VarDef {
                name: Spanned::new(var.to_string(), key_span),
                value: Spanned::new(value.to_string(), val_span),
            }
        } else if key == "source" {
            Item::Source {
                path: Spanned::new(value.to_string(), val_span),
            }
        } else {
            Item::Assign {
                key: Spanned::new(key.to_string(), key_span),
                value: Spanned::new(value.to_string(), val_span),
            }
        };

        items.push(item);
        cur.idx += 1;
    }

    if depth != 0 {
        let last = cur.lines.len().max(1);
        return Err(ParseError {
            message: "unclosed section: expected `}`".into(),
            span: Span::new(last, 1, 1),
        });
    }

    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assign(items: &[Item], key: &str) -> String {
        items
            .iter()
            .find_map(|i| match i {
                Item::Assign { key: k, value } if k.value == key => Some(value.value.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no assignment named `{key}`"))
    }

    fn section<'a>(items: &'a [Item], name: &str) -> &'a [Item] {
        items
            .iter()
            .find_map(|i| match i {
                Item::Section { name: n, items } if n.value == name => Some(items.as_slice()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no section named `{name}`"))
    }

    #[test]
    fn parses_flat_assignments() {
        let ast = parse("autotile = true\nrounding = 10\n").unwrap();
        assert_eq!(assign(&ast.items, "autotile"), "true");
        assert_eq!(assign(&ast.items, "rounding"), "10");
    }

    #[test]
    fn parses_variables() {
        let ast = parse("$accent = rgb(6b9fed)\n").unwrap();
        match &ast.items[0] {
            Item::VarDef { name, value } => {
                assert_eq!(name.value, "accent");
                assert_eq!(value.value, "rgb(6b9fed)");
            }
            other => panic!("expected VarDef, got {other:?}"),
        }
    }

    #[test]
    fn parses_nested_sections() {
        let src = "decoration {\n    rounding = 10\n    blur {\n        size = 6\n    }\n}\n";
        let ast = parse(src).unwrap();
        let deco = section(&ast.items, "decoration");
        assert_eq!(assign(deco, "rounding"), "10");
        assert_eq!(assign(section(deco, "blur"), "size"), "6");
    }

    #[test]
    fn strips_comments_but_keeps_values() {
        let ast = parse("gaps_in = 3   # inner gap\n# whole line\n").unwrap();
        assert_eq!(assign(&ast.items, "gaps_in"), "3");
        assert_eq!(ast.items.len(), 1);
    }

    #[test]
    fn source_is_its_own_item() {
        let ast = parse("source = ~/.config/hyprcosmic/monitors.conf\n").unwrap();
        match &ast.items[0] {
            Item::Source { path } => assert_eq!(path.value, "~/.config/hyprcosmic/monitors.conf"),
            other => panic!("expected Source, got {other:?}"),
        }
    }

    #[test]
    fn repeatable_keys_are_preserved_in_order() {
        let ast = parse("bind = SUPER, Return, spawn, kitty\nbind = SUPER, Q, close\n").unwrap();
        let binds: Vec<_> = ast
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Assign { key, value } if key.value == "bind" => Some(value.value.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(binds, vec!["SUPER, Return, spawn, kitty", "SUPER, Q, close"]);
    }

    #[test]
    fn spans_point_at_the_key() {
        let ast = parse("general {\n    gaps_inn = 8\n}\n").unwrap();
        let inner = section(&ast.items, "general");
        match &inner[0] {
            Item::Assign { key, .. } => {
                assert_eq!(key.span.line, 2);
                assert_eq!(key.span.col, 5);
                assert_eq!(key.span.len, "gaps_inn".len());
            }
            other => panic!("expected Assign, got {other:?}"),
        }
    }

    #[test]
    fn rejects_unclosed_section() {
        let err = parse("general {\n    gaps_in = 3\n").unwrap_err();
        assert!(err.message.contains("unclosed section"), "{}", err.message);
    }

    #[test]
    fn rejects_unmatched_brace() {
        let err = parse("}\n").unwrap_err();
        assert!(err.message.contains("unmatched"), "{}", err.message);
    }

    #[test]
    fn rejects_line_without_equals() {
        let err = parse("this is not valid\n").unwrap_err();
        assert!(err.message.contains("expected `key = value`"), "{}", err.message);
        assert_eq!(err.span.line, 1);
    }
}
