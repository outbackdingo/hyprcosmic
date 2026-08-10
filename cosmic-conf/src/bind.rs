//! Hyprland `bind` lines -> COSMIC shortcut bindings.
//!
//! `bind = SUPER, D, exec, rofi -show drun` is the single most recognisable
//! line in a hyprland.conf, so it is the one piece of the idiom that has to
//! feel native rather than translated.
//!
//! The target is the `custom` key of `com.system76.CosmicSettings.Shortcuts`,
//! which the compositor merges over `defaults`, letting a bind here override a
//! stock COSMIC shortcut without touching the system file
//! (cosmic-settings-daemon `config/src/shortcuts/mod.rs`: `shortcuts()` reads
//! `defaults`, then extends with `custom`).
//!
//! Actions are rendered as RON text rather than modelled as an enum. COSMIC's
//! `Action` has forty-odd variants and this crate deliberately does not link
//! the cosmic crates; mirroring the enum would mean re-copying it every time
//! upstream adds a variant, and the mapping table below only ever needs a few.

use std::fmt::Write as _;

use crate::parser::Span;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bind {
    /// COSMIC modifier names, deduplicated and in COSMIC's own order.
    pub mods: Vec<&'static str>,
    /// xkb keysym name. `None` is a modifier-only binding, which COSMIC
    /// supports and its defaults use for the launcher on bare Super.
    pub key: Option<String>,
    /// Pre-rendered RON, e.g. `Spawn("rofi -show drun")` or `Focus(Left)`.
    pub action: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindError {
    pub message: String,
    pub help: Option<String>,
    pub span: Span,
}

fn err(span: Span, message: impl Into<String>, help: Option<&str>) -> BindError {
    BindError {
        message: message.into(),
        help: help.map(str::to_string),
        span,
    }
}

/// Modifier spellings Hyprland accepts, longest first so that the greedy scan
/// below consumes `SUPERSHIFT` correctly rather than stopping at a prefix.
const MODIFIERS: &[(&str, &str)] = &[
    ("SUPERKEY", "Super"),
    ("CONTROL", "Ctrl"),
    ("SHIFT", "Shift"),
    ("SUPER", "Super"),
    ("LOGO", "Super"),
    ("MOD4", "Super"),
    ("MOD1", "Alt"),
    ("CTRL", "Ctrl"),
    ("META", "Super"),
    ("ALT", "Alt"),
    ("WIN", "Super"),
];

/// COSMIC writes modifiers in this order in its own defaults; matching it keeps
/// generated files diffable against hand-written ones.
const MODIFIER_ORDER: &[&str] = &["Super", "Ctrl", "Alt", "Shift"];

/// Hyprland allows `SUPER SHIFT`, `SUPER+SHIFT` and bare `SUPERSHIFT`, so
/// separators are stripped and the remainder is consumed greedily.
fn parse_modifiers(raw: &str, span: Span) -> Result<Vec<&'static str>, BindError> {
    let mut rest: String = raw
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '+' && *c != '_')
        .collect::<String>()
        .to_ascii_uppercase();

    let mut found: Vec<&'static str> = Vec::new();
    'outer: while !rest.is_empty() {
        for (spelling, cosmic) in MODIFIERS {
            if let Some(tail) = rest.strip_prefix(spelling) {
                if !found.contains(cosmic) {
                    found.push(cosmic);
                }
                rest = tail.to_string();
                continue 'outer;
            }
        }
        return Err(err(
            span,
            format!("unknown modifier `{rest}`"),
            Some("known modifiers: SUPER, CTRL, ALT, SHIFT"),
        ));
    }

    found.sort_by_key(|m| MODIFIER_ORDER.iter().position(|o| o == m).unwrap_or(usize::MAX));
    Ok(found)
}

/// Named keys whose xkb spelling differs from what a Hyprland user types.
///
/// Anything absent falls through unchanged, so exact keysyms such as
/// `XF86AudioRaiseVolume` keep working without needing an entry here.
const KEY_NAMES: &[(&str, &str)] = &[
    ("return", "Return"),
    ("enter", "Return"),
    ("escape", "Escape"),
    ("esc", "Escape"),
    ("tab", "Tab"),
    ("backspace", "BackSpace"),
    ("delete", "Delete"),
    ("insert", "Insert"),
    ("home", "Home"),
    ("end", "End"),
    ("pageup", "Prior"),
    ("pagedown", "Next"),
    ("left", "Left"),
    ("right", "Right"),
    ("up", "Up"),
    ("down", "Down"),
    ("print", "Print"),
];

/// COSMIC's defaults spell letters lowercase (`key: "q"`) and punctuation by
/// keysym name (`key: "slash"`), so normalise toward that.
fn normalize_key(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();

    if let Some((_, name)) = KEY_NAMES.iter().find(|(k, _)| *k == lower) {
        return Some((*name).to_string());
    }
    // Function keys are uppercase-F in xkb.
    if let Some(n) = lower.strip_prefix('f') {
        if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) {
            return Some(format!("F{n}"));
        }
    }
    if trimmed.len() == 1 && trimmed.chars().all(|c| c.is_ascii_alphabetic()) {
        return Some(lower);
    }
    Some(trimmed.to_string())
}

fn ron_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

fn direction(arg: &str, span: Span, dispatcher: &str) -> Result<&'static str, BindError> {
    Ok(match arg.trim().to_ascii_lowercase().as_str() {
        "l" | "left" => "Left",
        "r" | "right" => "Right",
        "u" | "up" => "Up",
        "d" | "down" => "Down",
        other => {
            return Err(err(
                span,
                format!("`{dispatcher}` needs a direction, got `{other}`"),
                Some("use l, r, u or d"),
            ))
        }
    })
}

fn workspace_index(arg: &str, span: Span, dispatcher: &str) -> Result<u8, BindError> {
    arg.trim().parse::<u8>().map_err(|_| {
        err(
            span,
            format!("`{dispatcher}` needs a workspace number, got `{}`", arg.trim()),
            Some("COSMIC addresses workspaces 1-255 by index"),
        )
    })
}

/// Translate a Hyprland dispatcher and its argument into RON for COSMIC's
/// `Action`.
///
/// Only dispatchers with a genuine COSMIC equivalent are mapped. A dispatcher
/// that merely looks similar is rejected instead of approximated, because a
/// keybinding that silently does the wrong thing is worse than one that fails
/// to compile.
fn action(dispatcher: &str, arg: &str, span: Span) -> Result<String, BindError> {
    let d = dispatcher.trim().to_ascii_lowercase();
    Ok(match d.as_str() {
        "exec" => {
            let cmd = arg.trim();
            if cmd.is_empty() {
                return Err(err(span, "`exec` needs a command", None));
            }
            // cosmic-comp runs this through `/bin/sh -c`
            // (`src/input/actions.rs`: `spawn_command`), so a full command line
            // with arguments and quoting behaves as written.
            format!("Spawn({})", ron_string(cmd))
        }
        "killactive" => "Close".into(),
        "fullscreen" => "Fullscreen".into(),
        "togglefloating" => "ToggleWindowFloating".into(),
        "togglesplit" => "ToggleOrientation".into(),
        "togglegroup" => "ToggleStacking".into(),
        "pin" => "ToggleSticky".into(),
        "exit" => "System(LogOut)".into(),
        "movefocus" => format!("Focus({})", direction(arg, span, &d)?),
        "movewindow" => format!("Move({})", direction(arg, span, &d)?),
        "workspace" => format!("Workspace({})", workspace_index(arg, span, &d)?),
        "movetoworkspace" => format!("MoveToWorkspace({})", workspace_index(arg, span, &d)?),
        "movetoworkspacesilent" => format!("SendToWorkspace({})", workspace_index(arg, span, &d)?),
        "focusmonitor" => format!("SwitchOutput({})", direction(arg, span, &d)?),
        "movewindowtomonitor" => format!("MoveToOutput({})", direction(arg, span, &d)?),

        // Present in Hyprland, absent from COSMIC. Named explicitly so the
        // error says why rather than "unknown".
        "pseudo" | "forcerendererreload" | "submap" | "toggleopaque" | "centerwindow"
        | "splitratio" | "cyclenext" | "swapnext" => {
            return Err(err(
                span,
                format!("`{d}` has no COSMIC equivalent"),
                Some("remove the bind, or use `exec` to run a program instead"),
            ))
        }
        other => {
            return Err(err(
                span,
                format!("unknown dispatcher `{other}`"),
                Some("supported: exec, killactive, fullscreen, togglefloating, togglesplit, movefocus, movewindow, workspace, movetoworkspace, exit"),
            ))
        }
    })
}

/// Parse the value of one `bind = ...` line.
///
/// Shape is `MODS, KEY, dispatcher, args`, with args keeping any further
/// commas, since `exec` commands routinely contain them.
pub fn parse_bind(value: &str, span: Span) -> Result<Bind, BindError> {
    let parts: Vec<&str> = value.splitn(4, ',').collect();
    if parts.len() < 3 {
        return Err(err(
            span,
            "a bind needs at least MODS, KEY and a dispatcher",
            Some("for example: bind = SUPER, D, exec, rofi -show drun"),
        ));
    }

    let mods = parse_modifiers(parts[0], span)?;
    let key = normalize_key(parts[1]);
    if mods.is_empty() && key.is_none() {
        return Err(err(span, "a bind needs a modifier or a key", None));
    }

    let arg = parts.get(3).copied().unwrap_or("");
    let action = action(parts[2], arg, span)?;

    Ok(Bind { mods, key, action })
}

/// Render the collected binds as the RON map COSMIC stores in `custom`.
pub fn render(binds: &[Bind]) -> String {
    let mut out = String::from("{\n");
    for b in binds {
        let mods = b
            .mods
            .iter()
            .map(|m| m.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        match &b.key {
            // `key` is `skip_serializing_if = "Option::is_none"` on COSMIC's
            // `Binding`, and its own defaults omit it for `(modifiers: [Super])`.
            Some(k) => {
                let _ = writeln!(
                    out,
                    "    (modifiers: [{mods}], key: {}): {},",
                    ron_string(k),
                    b.action
                );
            }
            None => {
                let _ = writeln!(out, "    (modifiers: [{mods}]): {},", b.action);
            }
        }
    }
    out.push_str("}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Span = Span {
        line: 1,
        col: 1,
        len: 1,
    };

    fn bind(v: &str) -> Bind {
        parse_bind(v, S).expect("should parse")
    }

    #[test]
    fn the_canonical_hyprland_launcher_bind() {
        let b = bind("SUPER, D, exec, rofi -show drun");
        assert_eq!(b.mods, vec!["Super"]);
        assert_eq!(b.key.as_deref(), Some("d"));
        assert_eq!(b.action, r#"Spawn("rofi -show drun")"#);
    }

    #[test]
    fn modifiers_accept_every_separator_hyprland_does() {
        for spelling in ["SUPER SHIFT", "SUPER+SHIFT", "SUPERSHIFT", "super shift"] {
            assert_eq!(
                bind(&format!("{spelling}, Q, killactive")).mods,
                vec!["Super", "Shift"],
                "failed for `{spelling}`"
            );
        }
    }

    #[test]
    fn modifiers_are_ordered_like_cosmics_own_defaults() {
        assert_eq!(
            bind("SHIFT ALT CTRL SUPER, Q, killactive").mods,
            vec!["Super", "Ctrl", "Alt", "Shift"]
        );
    }

    #[test]
    fn a_bind_with_no_key_is_modifier_only() {
        // COSMIC's defaults bind bare Super to the launcher this way.
        let b = bind("SUPER, , exec, rofi -show drun");
        assert_eq!(b.key, None);
        assert_eq!(render(&[b]), "{\n    (modifiers: [Super]): Spawn(\"rofi -show drun\"),\n}\n");
    }

    #[test]
    fn keys_normalise_to_xkb_spelling() {
        assert_eq!(bind("SUPER, Q, killactive").key.as_deref(), Some("q"));
        assert_eq!(bind("SUPER, Return, killactive").key.as_deref(), Some("Return"));
        assert_eq!(bind("SUPER, enter, killactive").key.as_deref(), Some("Return"));
        assert_eq!(bind("SUPER, f5, killactive").key.as_deref(), Some("F5"));
        assert_eq!(bind("SUPER, slash, killactive").key.as_deref(), Some("slash"));
        // Unknown names pass through so exact keysyms stay usable.
        assert_eq!(
            bind("SUPER, XF86AudioRaiseVolume, killactive").key.as_deref(),
            Some("XF86AudioRaiseVolume")
        );
    }

    #[test]
    fn exec_keeps_commas_in_the_command() {
        assert_eq!(
            bind("SUPER, E, exec, sh -c 'echo a, b'").action,
            r#"Spawn("sh -c 'echo a, b'")"#
        );
    }

    #[test]
    fn quotes_in_a_command_are_escaped_not_emitted_raw() {
        // Otherwise the generated RON would not parse.
        assert_eq!(
            bind(r#"SUPER, E, exec, echo "hi""#).action,
            r#"Spawn("echo \"hi\"")"#
        );
    }

    #[test]
    fn dispatchers_map_to_cosmic_actions() {
        assert_eq!(bind("SUPER, Q, killactive").action, "Close");
        assert_eq!(bind("SUPER, F, fullscreen").action, "Fullscreen");
        assert_eq!(bind("SUPER, left, movefocus, l").action, "Focus(Left)");
        assert_eq!(bind("SUPER SHIFT, left, movewindow, l").action, "Move(Left)");
        assert_eq!(bind("SUPER, 1, workspace, 1").action, "Workspace(1)");
        assert_eq!(
            bind("SUPER SHIFT, 1, movetoworkspace, 1").action,
            "MoveToWorkspace(1)"
        );
    }

    #[test]
    fn a_dispatcher_without_an_equivalent_is_refused_not_approximated() {
        let e = parse_bind("SUPER, P, pseudo", S).unwrap_err();
        assert!(e.message.contains("no COSMIC equivalent"), "{}", e.message);
        assert!(e.help.is_some());
    }

    #[test]
    fn unknown_dispatchers_and_modifiers_are_reported() {
        assert!(parse_bind("SUPER, X, frobnicate", S)
            .unwrap_err()
            .message
            .contains("unknown dispatcher"));
        assert!(parse_bind("HYPER, X, killactive", S)
            .unwrap_err()
            .message
            .contains("unknown modifier"));
    }

    #[test]
    fn a_truncated_bind_says_what_shape_is_expected() {
        let e = parse_bind("SUPER, D", S).unwrap_err();
        assert!(e.help.unwrap().contains("rofi -show drun"));
    }

    #[test]
    fn rendering_matches_the_shape_cosmic_writes_in_its_defaults() {
        let out = render(&[
            bind("SUPER, , exec, rofi -show drun"),
            bind("SUPER, slash, exec, rofi -show drun"),
            bind("SUPER SHIFT, Q, killactive"),
        ]);
        assert_eq!(
            out,
            concat!(
                "{\n",
                "    (modifiers: [Super]): Spawn(\"rofi -show drun\"),\n",
                "    (modifiers: [Super], key: \"slash\"): Spawn(\"rofi -show drun\"),\n",
                "    (modifiers: [Super, Shift], key: \"q\"): Close,\n",
                "}\n"
            )
        );
    }
}
