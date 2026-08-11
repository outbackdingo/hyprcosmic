//! Hyprland `windowrule` lines -> COSMIC window rules.
//!
//! Hyprland's `windowrule` is a large surface: an action, a match, and around
//! forty possible actions ranging from `float` to `bordercolor`. Exactly one of
//! them is implemented here, `workspace`, because it is the one with a real
//! COSMIC counterpart -- a window can be mapped onto a workspace other than the
//! active one, which is what `windowrule = workspace 4, class:...` means.
//!
//! Everything else is refused with an explanation rather than accepted and
//! dropped. A rule that parses and then does nothing is the worst outcome
//! available: the config looks right, the window opens in the wrong place, and
//! there is nothing to read that says why.
//!
//! The match half is `class:` and `title:`, both regular expressions, both
//! compiled here so a broken one is a diagnostic against the line that wrote it
//! rather than a warning in the compositor log nobody reads.

use std::fmt::Write as _;

use crate::parser::Span;

/// Where a matching window opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceTarget {
    /// 1-based, as the user counts them.
    Index(u32),
    /// Matched against the workspace name, which is what a `workspace` line
    /// sets.
    Name(String),
}

/// One `windowrule = ...` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowRuleDecl {
    /// Regular expression for the window's app id. Empty matches anything.
    pub class: String,
    /// Regular expression for the window's title. Empty matches anything.
    pub title: String,
    pub workspace: WorkspaceTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowRuleError {
    pub message: String,
    pub help: Option<String>,
    pub span: Span,
}

fn err(span: Span, message: impl Into<String>, help: Option<&str>) -> WindowRuleError {
    WindowRuleError {
        message: message.into(),
        help: help.map(str::to_string),
        span,
    }
}

/// The actions Hyprland has that this cannot do, and why saying so beats
/// guessing. `float` and `tile` are called out separately because they are the
/// next most likely thing to be reached for and COSMIC does have a mechanism --
/// just not one cosmic.conf owns.
const FLOAT_HELP: &str = "cosmic-comp decides floating from its tiling exceptions, which belong to \
     cosmic-settings (com.system76.CosmicSettings.WindowRules) rather than to \
     cosmic.conf. Add the application there and it will float on every \
     workspace.";

const ACTION_HELP: &str = "only `workspace` is supported. Hyprland's other rules -- float, size, \
     move, opacity, bordercolor and the rest -- have no COSMIC equivalent to \
     project onto.";

/// Matchers Hyprland has that depend on window state at match time. Ours runs
/// once, when the window is mapped, so none of these can be answered.
const MATCHER_HELP: &str = "rules are matched once, as the window opens, so only what the window \
     arrives with can be tested: class and title.";

/// Parse the body of a `windowrule` line.
///
/// `windowrule = workspace 4, class:^(vivaldi.*)$`
///
/// The first field is the action, the rest are matchers. `windowrulev2` is the
/// same thing under an older name -- Hyprland merged v2's syntax into
/// `windowrule` and kept the alias, and HyDE-era configs are full of it.
pub fn parse_window_rule(value: &str, span: Span) -> Result<WindowRuleDecl, WindowRuleError> {
    let mut parts = value.split(',');

    let action = parts
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            err(
                span,
                "a window rule needs an action",
                Some("for example: windowrule = workspace 4, class:^(vivaldi)$"),
            )
        })?;

    let workspace = parse_action(action, span)?;

    let mut class = String::new();
    let mut title = String::new();
    let mut matched_on = false;

    for raw in parts {
        let param = raw.trim();
        if param.is_empty() {
            continue;
        }
        let Some((key, arg)) = param.split_once(':') else {
            return Err(err(
                span,
                format!("expected `key:value`, found `{param}`"),
                Some("known matchers: class, title"),
            ));
        };
        let arg = arg.trim();
        // `initialClass`/`initialTitle` are accepted as spellings of the same
        // thing rather than as approximations of it: the match happens as the
        // window is mapped, so the title being tested *is* the initial one.
        match key.trim().to_ascii_lowercase().as_str() {
            "class" | "initialclass" => {
                check_regex(arg, "class", span)?;
                class = arg.to_string();
                matched_on = true;
            }
            "title" | "initialtitle" => {
                check_regex(arg, "title", span)?;
                title = arg.to_string();
                matched_on = true;
            }
            // Named rather than swept into the catch-all so the message can say
            // why a matcher Hyprland does have is not accepted here.
            "floating" | "fullscreen" | "pinned" | "focus" | "workspace" | "onworkspace"
            | "xwayland" | "tag" | "fullscreenstate" => {
                return Err(err(
                    span,
                    format!("`{}:` cannot be matched on", key.trim()),
                    Some(MATCHER_HELP),
                ));
            }
            other => {
                return Err(err(
                    span,
                    format!("unknown matcher `{other}`"),
                    Some("known matchers: class, title"),
                ));
            }
        }
    }

    if !matched_on {
        return Err(err(
            span,
            "a window rule needs something to match on",
            Some(
                "without a class or a title the rule matches every window, which \
                 would send the whole session to one workspace.",
            ),
        ));
    }

    Ok(WindowRuleDecl {
        class,
        title,
        workspace,
    })
}

/// `workspace 4`, `workspace name:web`, either with a trailing `silent`.
fn parse_action(action: &str, span: Span) -> Result<WorkspaceTarget, WindowRuleError> {
    let (verb, rest) = match action.split_once(char::is_whitespace) {
        Some((verb, rest)) => (verb, rest.trim()),
        None => (action, ""),
    };
    if !verb.eq_ignore_ascii_case("workspace") {
        return Err(err(
            span,
            format!("unsupported window rule `{verb}`"),
            Some(
                if verb.eq_ignore_ascii_case("float") || verb.eq_ignore_ascii_case("tile") {
                    FLOAT_HELP
                } else {
                    ACTION_HELP
                },
            ),
        ));
    }

    // Hyprland's `silent` means "put it there without switching to it". That is
    // unconditionally what happens here -- a rule places its window and leaves
    // the focus alone -- so the word is accepted as a description of the
    // behaviour rather than ignored as a request that went unheard.
    //
    // Stripped from the end rather than parsed as one word among several,
    // because a workspace name may contain spaces: `workspace = 2, name:web and
    // mail` is a legal declaration, so `workspace name:web and mail` has to be a
    // legal rule.
    let target = rest
        .rsplit_once(char::is_whitespace)
        .filter(|(_, last)| last.eq_ignore_ascii_case("silent"))
        .map_or(rest, |(head, _)| head.trim_end());

    if target.is_empty() {
        return Err(err(
            span,
            "`workspace` needs a workspace to send the window to",
            Some("for example: workspace 4, or workspace name:web"),
        ));
    }

    if let Some(name) = target.strip_prefix("name:") {
        if name.is_empty() {
            return Err(err(span, "`name:` needs a workspace name", None));
        }
        return Ok(WorkspaceTarget::Name(name.to_string()));
    }

    // Digits checked before parsing, not after: `u32::from_str` accepts a
    // leading `+`, so `workspace +1` -- Hyprland's "one to the right" -- would
    // otherwise parse as the absolute workspace 1 and send the window somewhere
    // the rule never asked for.
    if target.bytes().all(|b| b.is_ascii_digit()) {
        match target.parse::<u32>() {
            Ok(0) => return Err(err(span, "workspaces are numbered from 1", None)),
            Ok(index) => return Ok(WorkspaceTarget::Index(index)),
            // Only reachable by overflow, which the message below covers.
            Err(_) => {}
        }
    }

    // Everything else Hyprland accepts here is relative to where you are --
    // `+1`, `previous`, `empty`, `special` -- and a rule fires when a window
    // opens, so "the next workspace" would mean a different one every time.
    Err(err(
        span,
        format!("cannot send a window to `{target}`"),
        Some(
            "a rule names one fixed workspace: a number, or `name:` and the name \
             from a `workspace` line, optionally followed by `silent`. Relative \
             and special workspaces have no COSMIC equivalent.",
        ),
    ))
}

fn check_regex(pattern: &str, field: &str, span: Span) -> Result<(), WindowRuleError> {
    if pattern.is_empty() {
        return Err(err(
            span,
            format!("`{field}:` needs a value"),
            Some("an empty expression matches every window; leave the matcher out instead."),
        ));
    }
    regex::Regex::new(pattern).map_err(|e| {
        // The crate's own message is multi-line and already points at the
        // offending character, which is more useful than anything paraphrased.
        err(
            span,
            format!("`{field}:` is not a valid expression"),
            Some(&e.to_string().replace('\n', " ")),
        )
    })?;
    Ok(())
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

/// Render the declarations as the RON `Vec<WindowRule>` cosmic-comp stores in
/// `window_rules`.
///
/// Order is preserved because it is meaningful: the compositor takes the first
/// rule that matches, and a file reads top to bottom, so the earlier line is
/// the one a person expects to win.
pub fn render(decls: &[WindowRuleDecl]) -> String {
    let mut out = String::from("[\n");
    for decl in decls {
        let workspace = match &decl.workspace {
            WorkspaceTarget::Index(n) => format!("Index({n})"),
            WorkspaceTarget::Name(name) => format!("Name({})", ron_string(name)),
        };
        let _ = writeln!(
            out,
            "    (app_id: {}, title: {}, workspace: {workspace}),",
            ron_string(&decl.class),
            ron_string(&decl.title),
        );
    }
    out.push_str("]\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span() -> Span {
        Span {
            line: 1,
            col: 1,
            len: 1,
        }
    }

    fn ok(s: &str) -> WindowRuleDecl {
        parse_window_rule(s, span()).expect(s)
    }

    fn fail(s: &str) -> WindowRuleError {
        parse_window_rule(s, span()).expect_err(s)
    }

    #[test]
    fn the_hyprland_form_parses() {
        let r = ok("workspace 4, class:^(vivaldi.*)$");
        assert_eq!(r.workspace, WorkspaceTarget::Index(4));
        assert_eq!(r.class, "^(vivaldi.*)$");
        assert_eq!(r.title, "");
    }

    #[test]
    fn a_named_workspace_is_carried_through() {
        assert_eq!(
            ok("workspace name:web, class:vivaldi").workspace,
            WorkspaceTarget::Name("web".into())
        );
    }

    #[test]
    fn class_and_title_can_both_be_given() {
        let r = ok("workspace 2, class:^(firefox)$, title:.*Mail.*");
        assert_eq!(r.class, "^(firefox)$");
        assert_eq!(r.title, ".*Mail.*");
    }

    #[test]
    fn a_title_alone_is_enough_to_match_on() {
        let r = ok("workspace 2, title:.*Mail.*");
        assert_eq!(r.class, "", "an empty class matches every app id");
        assert_eq!(r.title, ".*Mail.*");
    }

    #[test]
    fn initial_spellings_are_the_same_matchers() {
        let r = ok("workspace 1, initialClass:foo, initialTitle:bar");
        assert_eq!(r.class, "foo");
        assert_eq!(r.title, "bar");
    }

    #[test]
    fn silent_is_accepted_because_it_describes_what_happens() {
        assert_eq!(
            ok("workspace 3 silent, class:foo").workspace,
            WorkspaceTarget::Index(3)
        );
    }

    #[test]
    fn case_does_not_matter_for_keywords() {
        assert_eq!(
            ok("Workspace 3 SILENT, CLASS:foo").workspace,
            WorkspaceTarget::Index(3)
        );
    }

    #[test]
    fn whitespace_around_everything_is_tolerated() {
        let r = ok("  workspace  4 ,  class : ^foo$  ");
        assert_eq!(r.workspace, WorkspaceTarget::Index(4));
        assert_eq!(r.class, "^foo$");
    }

    #[test]
    fn a_rule_with_nothing_to_match_on_is_refused() {
        let e = fail("workspace 4");
        assert!(e.message.contains("something to match on"), "{e:?}");
    }

    #[test]
    fn an_unsupported_action_says_which_one_is_supported() {
        let e = fail("size 100 100, class:foo");
        assert!(e.message.contains("unsupported window rule"), "{e:?}");
        assert!(e.help.unwrap().contains("only `workspace`"));
    }

    #[test]
    fn float_points_at_the_tiling_exceptions_instead() {
        let e = fail("float, class:foo");
        assert!(
            e.help.as_deref().unwrap_or_default().contains("cosmic-settings"),
            "{e:?}"
        );
    }

    #[test]
    fn a_state_matcher_explains_that_matching_happens_once() {
        let e = fail("workspace 4, class:foo, floating:1");
        assert!(e.message.contains("cannot be matched on"), "{e:?}");
        assert!(e.help.unwrap().contains("as the window opens"));
    }

    #[test]
    fn an_unknown_matcher_lists_the_known_ones() {
        let e = fail("workspace 4, klass:foo");
        assert!(e.message.contains("unknown matcher"), "{e:?}");
    }

    /// `u32::from_str` accepts a leading sign, so `+1` would silently become
    /// the absolute workspace 1 if the digits were not checked first.
    #[test]
    fn a_relative_workspace_is_refused_with_a_reason() {
        for target in ["+1", "-1", "previous", "empty", "e+1"] {
            let e = fail(&format!("workspace {target}, class:foo"));
            assert!(
                e.message.contains("cannot send a window to"),
                "{target}: {}",
                e.message
            );
            assert!(
                e.help.as_deref().unwrap_or_default().contains("one fixed workspace"),
                "{target}: {:?}",
                e.help
            );
        }
    }

    #[test]
    fn a_special_workspace_is_refused() {
        assert!(fail("workspace special:magic, class:foo")
            .message
            .contains("cannot send a window to"));
    }

    #[test]
    fn workspace_zero_is_refused() {
        assert!(fail("workspace 0, class:foo")
            .message
            .contains("numbered from 1"));
    }

    #[test]
    fn a_broken_expression_is_caught_here_not_in_the_compositor() {
        let e = fail("workspace 4, class:^(unclosed");
        assert!(e.message.contains("not a valid expression"), "{e:?}");
        assert!(e.help.is_some(), "the regex crate's own message is passed on");
    }

    #[test]
    fn an_empty_matcher_is_refused_rather_than_matching_everything() {
        let e = fail("workspace 4, class:");
        assert!(e.message.contains("needs a value"), "{e:?}");
    }

    /// `silent` is stripped off the end, so anything else trailing an index is
    /// part of the target and fails as one rather than being quietly dropped.
    #[test]
    fn a_modifier_that_is_not_silent_is_refused() {
        let e = fail("workspace 4 loud, class:foo");
        assert!(e.message.contains("cannot send a window to `4 loud`"), "{e:?}");
    }

    /// A `workspace` line accepts a name with spaces in it, so a rule aiming at
    /// that workspace has to as well.
    #[test]
    fn a_workspace_name_may_contain_spaces() {
        assert_eq!(
            ok("workspace name:web and mail, class:foo").workspace,
            WorkspaceTarget::Name("web and mail".into())
        );
        assert_eq!(
            ok("workspace name:web and mail silent, class:foo").workspace,
            WorkspaceTarget::Name("web and mail".into())
        );
    }

    #[test]
    fn workspace_with_nothing_after_it_says_so() {
        let e = fail("workspace, class:foo");
        assert!(e.message.contains("needs a workspace"), "{e:?}");
    }

    #[test]
    fn rendering_matches_the_ron_shape_cosmic_comp_reads() {
        let out = render(&[
            WindowRuleDecl {
                class: "^(vivaldi)$".into(),
                title: String::new(),
                workspace: WorkspaceTarget::Name("web".into()),
            },
            WindowRuleDecl {
                class: "^(kitty)$".into(),
                title: String::new(),
                workspace: WorkspaceTarget::Index(1),
            },
        ]);

        assert_eq!(
            out,
            concat!(
                "[\n",
                "    (app_id: \"^(vivaldi)$\", title: \"\", workspace: Name(\"web\")),\n",
                "    (app_id: \"^(kitty)$\", title: \"\", workspace: Index(1)),\n",
                "]\n",
            )
        );
    }

    #[test]
    fn nothing_renders_as_an_empty_list() {
        assert_eq!(render(&[]), "[\n]\n");
    }

    #[test]
    fn a_quote_in_an_expression_cannot_break_out_of_the_ron() {
        let out = render(&[WindowRuleDecl {
            class: r#"^(say "hi")$"#.into(),
            title: String::new(),
            workspace: WorkspaceTarget::Index(1),
        }]);
        assert!(out.contains(r#"\"hi\""#), "{out}");
    }
}
