//! Hyprland `workspace` lines -> COSMIC pinned workspaces.
//!
//! COSMIC's workspaces are dynamic and unconditionally so: `ensure_last_empty`
//! keeps exactly one trailing empty workspace and garbage-collects every other
//! empty one, and there is no setting anywhere that turns that off. A Hyprland
//! user expects the opposite -- a fixed set of workspaces that exist whether or
//! not anything is on them, so that "workspace 4 is the browser" stays true
//! across a reboot.
//!
//! The primitive that bridges the two already exists in the compositor.
//! `Workspace::can_auto_remove` is `is_empty() && !has_activation_token() &&
//! !pinned`, so a pinned workspace survives being emptied, and
//! `CosmicCompConfig::pinned_workspaces` is a persisted key that
//! `Workspaces::add_output` drains into the first output that appears. Nothing
//! in cosmic-comp needs to change: declaring workspaces here is enough.
//!
//! Three consequences of that restore path shape this module:
//!
//! 1. **Restore is positional.** `PinnedWorkspace` has no index field -- the
//!    order of the Vec becomes the order of the workspaces. So `workspace = 4`
//!    cannot emit one entry; it has to emit four, with 1..3 unnamed, or the
//!    declared workspace would land at index 1.
//! 2. **The trailing dynamic workspace is kept.** Pinned workspaces are pushed
//!    into an empty `WorkspaceSet`, and `ensure_last_empty` then appends the
//!    usual empty one. Declaring four leaves you on 1-4 with a fifth appearing
//!    when you use it, which is Hyprland's behaviour rather than a compromise.
//! 3. **It lands at the next login, not on apply.** `Workspaces::new` reads the
//!    key once when the compositor starts and there is no reload path for it,
//!    while `hyprcosmic-conf watch` is started from the autostart file *after*
//!    COSMIC's own components. So an edit is written immediately and takes
//!    effect the next time the session starts. Every other key in cosmic.conf
//!    is live, so this one is worth saying out loud.

use std::fmt::Write as _;

use crate::parser::Span;

/// One `workspace = ...` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceDecl {
    /// 1-based, as written. Workspaces below this one are materialised too.
    pub index: u32,
    /// Shown by anything reading ext-workspace, waybar included. `None` leaves
    /// COSMIC to label it by number.
    pub name: Option<String>,
    /// Per-workspace tiling. `None` inherits the session default.
    pub tiling: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceError {
    pub message: String,
    pub help: Option<String>,
    pub span: Span,
}

fn err(span: Span, message: impl Into<String>, help: Option<&str>) -> WorkspaceError {
    WorkspaceError {
        message: message.into(),
        help: help.map(str::to_string),
        span,
    }
}

/// Declaring index N materialises N workspaces, so a typo like `workspace = 100`
/// would silently produce a hundred of them. Nobody drives a hundred
/// workspaces; the cap turns a fat-finger into a diagnostic.
const MAX_INDEX: u32 = 32;

/// Why `monitor:` is refused rather than accepted and ignored.
///
/// A `PinnedWorkspace` names its output through `OutputMatch { name, edid }`,
/// and cosmic-comp's `output_matches` compares the EDID *first*: a match with
/// `edid: None` is rejected outright against any output that reports one, and
/// only falls through to the name when neither side has an EDID. Every real
/// panel reports one, so a name-only match would work on a VM and nowhere else.
///
/// cosmic.conf cannot supply the EDID -- it is a manufacturer triple, product
/// id, serial and manufacture date read off the wire by the DRM backend, not
/// something a user can write down. Accepting `monitor:` would give a parameter
/// that silently does nothing on the hardware people actually have.
const MONITOR_HELP: &str = "COSMIC matches a workspace to a monitor by EDID rather than by name, \
     and cosmic.conf has no way to spell an EDID. Pinned workspaces are created \
     on the first output the session sees; move them with the workspace \
     shortcuts once they exist.";

/// Parse the body of a `workspace` line.
///
/// `workspace = 4, name:web, tiling:true`
///
/// The index comes first and is required. Hyprland also allows a leading
/// `name:` with no index, for its special workspaces; COSMIC has no equivalent
/// and the restore is positional besides, so that form is rejected with an
/// explanation rather than guessed at.
pub fn parse_workspace(value: &str, span: Span) -> Result<WorkspaceDecl, WorkspaceError> {
    let mut parts = value.split(',');

    let head = parts
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            err(
                span,
                "a workspace needs an index",
                Some("for example: workspace = 4, name:web"),
            )
        })?;

    let index = head.parse::<u32>().map_err(|_| {
        if head.contains(':') {
            err(
                span,
                format!("a workspace starts with its index, found `{head}`"),
                Some(
                    "COSMIC restores pinned workspaces by position, so every one \
                     needs a number. Write `workspace = 4, name:web` rather than \
                     `workspace = name:web`.",
                ),
            )
        } else {
            err(
                span,
                format!("expected a workspace index, found `{head}`"),
                Some("for example: workspace = 4, name:web"),
            )
        }
    })?;

    if index == 0 || index > MAX_INDEX {
        return Err(err(
            span,
            format!("workspace index {index} is outside 1..={MAX_INDEX}"),
            Some(
                "workspaces are numbered from 1, and declaring one materialises \
                 every workspace below it, so the highest index is capped.",
            ),
        ));
    }

    let mut decl = WorkspaceDecl {
        index,
        name: None,
        tiling: None,
    };

    for raw in parts {
        let param = raw.trim();
        if param.is_empty() {
            continue;
        }
        let Some((key, arg)) = param.split_once(':') else {
            return Err(err(
                span,
                format!("expected `key:value`, found `{param}`"),
                Some("known parameters: name, tiling"),
            ));
        };
        let arg = arg.trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "name" => {
                if arg.is_empty() {
                    return Err(err(span, "`name:` needs a value", None));
                }
                decl.name = Some(arg.to_string());
            }
            // Named rather than swept into the catch-all so the message can say
            // why a parameter Hyprland does have is not accepted here.
            "monitor" | "output" => {
                return Err(err(
                    span,
                    format!("`{}:` has no COSMIC equivalent", key.trim()),
                    Some(MONITOR_HELP),
                ));
            }
            "tiling" => {
                decl.tiling = Some(match arg.to_ascii_lowercase().as_str() {
                    "true" | "yes" | "on" | "1" => true,
                    "false" | "no" | "off" | "0" => false,
                    other => {
                        return Err(err(
                            span,
                            format!("`tiling:` expects a boolean, found `{other}`"),
                            None,
                        ))
                    }
                });
            }
            other => {
                return Err(err(
                    span,
                    format!("unknown workspace parameter `{other}`"),
                    Some("known parameters: name, tiling"),
                ));
            }
        }
    }

    Ok(decl)
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

/// FNV-1a, written out rather than reached for.
///
/// `DefaultHasher` is explicitly documented as not stable across Rust releases,
/// which would make workspace ids change under the user when the toolchain
/// moves -- and the id is what ties a window's saved workspace to the workspace
/// it reappears on. Twelve lines of FNV is cheaper than that class of bug.
fn fnv1a(s: &str) -> u32 {
    let mut h: u32 = 0x811c9dc5;
    for b in s.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x01000193);
    }
    h
}

/// A stable id for the workspace at `index`.
///
/// cosmic-comp's own `random_workspace_id` is `format!("{:x}", rand(0..2<<24))`,
/// which is at most seven hex digits. Forcing the high nibble on here makes ours
/// exactly eight, so a generated id and a compositor-generated one cannot
/// collide by construction rather than by being unlikely to.
fn workspace_id(index: u32) -> String {
    let h = fnv1a(&format!("hyprcosmic:workspace:{index}"));
    format!("{:08x}", 0x1000_0000 | (h & 0x0fff_ffff))
}

/// Render the declarations as the RON `Vec<PinnedWorkspace>` cosmic-comp stores
/// in `pinned_workspaces`.
///
/// `default_tiling` is the session-wide `general:autotile`, used for any
/// workspace that did not say. Without it, declaring workspaces would silently
/// turn tiling off on all of them for a user who had asked for it globally.
pub fn render(decls: &[WorkspaceDecl], default_tiling: bool) -> String {
    let highest = decls.iter().map(|d| d.index).max().unwrap_or(0);

    let mut out = String::from("[\n");
    for index in 1..=highest {
        // Gaps are filled rather than skipped: the restore is positional, so an
        // absent workspace 3 would put the declared workspace 4 at position 3.
        let decl = decls.iter().find(|d| d.index == index);

        let tiling = decl.and_then(|d| d.tiling).unwrap_or(default_tiling);
        let name = match decl.and_then(|d| d.name.as_deref()) {
            Some(n) => format!("Some({})", ron_string(n)),
            None => "None".to_string(),
        };

        // An empty `OutputMatch` is "no preference", not a guess. It is the
        // front of the workspace's `output_stack`, which `prefers_output`
        // consults when an output appears; a name nothing can match leaves the
        // workspace where `add_output` created it. See MONITOR_HELP.
        let _ = writeln!(
            out,
            r#"    (output: (name: "", edid: None), tiling_enabled: {tiling}, id: Some({}), name: {name}),"#,
            ron_string(&workspace_id(index)),
        );
    }
    out.push_str("]\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    const S: Span = Span {
        line: 1,
        col: 1,
        len: 1,
    };

    fn ws(v: &str) -> WorkspaceDecl {
        parse_workspace(v, S).expect("should parse")
    }

    fn bad(v: &str) -> WorkspaceError {
        parse_workspace(v, S).expect_err("should not parse")
    }

    #[test]
    fn the_shortest_useful_declaration_is_a_bare_index() {
        assert_eq!(
            ws("4"),
            WorkspaceDecl {
                index: 4,
                name: None,
                tiling: None,
            }
        );
    }

    #[test]
    fn every_parameter_together() {
        assert_eq!(
            ws("4, name:web, tiling:true"),
            WorkspaceDecl {
                index: 4,
                name: Some("web".into()),
                tiling: Some(true),
            }
        );
    }

    #[test]
    fn parameters_are_order_independent_and_tolerate_spacing() {
        assert_eq!(ws("2,tiling:off,name:mail"), ws("2, name:mail, tiling:off"));
    }

    /// Hyprland's `monitor:` is the parameter a user is most likely to reach
    /// for, and the one COSMIC cannot honour. Accepting and ignoring it would
    /// be the worst of the three options, so it must fail and say why.
    #[test]
    fn monitor_is_refused_with_the_reason_rather_than_silently_ignored() {
        for spelling in ["monitor", "output"] {
            let e = bad(&format!("1, {spelling}:eDP-1"));
            assert!(
                e.message.contains("no COSMIC equivalent"),
                "{spelling}: {}",
                e.message
            );
            assert!(
                e.help.as_deref().unwrap_or_default().contains("EDID"),
                "{spelling}: {:?}",
                e.help
            );
        }
    }

    #[test]
    fn a_name_may_contain_spaces() {
        assert_eq!(ws("1, name:web and mail").name.as_deref(), Some("web and mail"));
    }

    /// Hyprland's named-workspace form has no COSMIC equivalent, and the failure
    /// if it were guessed at would be workspaces in the wrong order.
    #[test]
    fn the_hyprland_name_first_form_is_refused_with_the_reason() {
        let e = bad("name:web, tiling:true");
        assert!(e.message.contains("starts with its index"), "{}", e.message);
        assert!(
            e.help.as_deref().unwrap_or_default().contains("by position"),
            "{:?}",
            e.help
        );
    }

    #[test]
    fn workspace_zero_does_not_exist() {
        let e = bad("0");
        assert!(e.message.contains("outside 1..="), "{}", e.message);
    }

    /// The cap is the whole reason this check exists: without it the typo below
    /// creates a thousand workspaces rather than reporting anything.
    #[test]
    fn an_absurd_index_is_a_diagnostic_not_a_thousand_workspaces() {
        let e = bad("1000");
        assert!(e.message.contains("outside 1..=32"), "{}", e.message);
    }

    #[test]
    fn an_unknown_parameter_lists_the_known_ones() {
        let e = bad("1, gapsin:0");
        assert!(e.message.contains("gapsin"), "{}", e.message);
        assert!(
            e.help.as_deref().unwrap_or_default().contains("name, tiling"),
            "{:?}",
            e.help
        );
    }

    #[test]
    fn a_parameter_without_a_colon_is_rejected() {
        let e = bad("1, web");
        assert!(e.message.contains("key:value"), "{}", e.message);
    }

    #[test]
    fn tiling_takes_the_same_boolean_spellings_as_the_rest_of_the_file() {
        for v in ["true", "yes", "on", "1"] {
            assert_eq!(ws(&format!("1, tiling:{v}")).tiling, Some(true), "{v}");
        }
        for v in ["false", "no", "off", "0"] {
            assert_eq!(ws(&format!("1, tiling:{v}")).tiling, Some(false), "{v}");
        }
        assert!(bad("1, tiling:sometimes").message.contains("boolean"));
    }

    /// The property the positional restore turns on: declaring only 4 must still
    /// emit 1, 2 and 3, or the browser workspace comes back as workspace 1.
    #[test]
    fn declaring_one_high_index_materialises_every_workspace_below_it() {
        let ron = render(&[ws("4, name:web")], false);
        assert_eq!(ron.lines().filter(|l| l.contains("output:")).count(), 4, "{ron}");

        let lines: Vec<&str> = ron.lines().filter(|l| l.contains("output:")).collect();
        assert!(lines[0].contains("name: None"), "{}", lines[0]);
        assert!(lines[1].contains("name: None"), "{}", lines[1]);
        assert!(lines[2].contains("name: None"), "{}", lines[2]);
        assert!(lines[3].contains(r#"name: Some("web")"#), "{}", lines[3]);
    }

    #[test]
    fn gaps_between_declarations_are_filled_in_order() {
        let ron = render(&[ws("3, name:code"), ws("1, name:term")], false);
        let lines: Vec<&str> = ron.lines().filter(|l| l.contains("output:")).collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains(r#"name: Some("term")"#), "{}", lines[0]);
        assert!(lines[1].contains("name: None"), "{}", lines[1]);
        assert!(lines[2].contains(r#"name: Some("code")"#), "{}", lines[2]);
    }

    #[test]
    fn no_declarations_render_as_an_empty_list() {
        assert_eq!(render(&[], false), "[\n]\n");
    }

    /// A workspace that did not mention tiling must not quietly contradict
    /// `general:autotile`.
    #[test]
    fn unset_tiling_follows_the_session_default_both_ways() {
        assert!(render(&[ws("1")], true).contains("tiling_enabled: true"));
        assert!(render(&[ws("1")], false).contains("tiling_enabled: false"));
        // ...and an explicit value still wins over it.
        assert!(render(&[ws("1, tiling:false")], true).contains("tiling_enabled: false"));
    }

    /// Not a placeholder: an unmatchable `OutputMatch` is how a workspace says
    /// it has no output preference, which is the only thing cosmic.conf can
    /// truthfully express. See MONITOR_HELP.
    #[test]
    fn the_output_match_is_always_empty() {
        assert!(render(&[ws("1")], false).contains(r#"output: (name: "", edid: None)"#));
    }

    /// Ids have to survive a re-apply, or every `hyprcosmic-conf apply` would hand
    /// the same workspaces new identities.
    #[test]
    fn ids_are_stable_across_runs_and_unique_per_index() {
        assert_eq!(workspace_id(4), workspace_id(4));
        let ids: BTreeSet<String> = (1..=MAX_INDEX).map(workspace_id).collect();
        assert_eq!(ids.len(), MAX_INDEX as usize, "an index collided");
    }

    /// cosmic-comp's `random_workspace_id` is `{:x}` of a number below 2<<24,
    /// so it is never more than seven hex digits. Ours are always eight, which
    /// is what makes a collision impossible rather than unlikely.
    #[test]
    fn ids_cannot_collide_with_a_compositor_generated_one() {
        for i in 1..=MAX_INDEX {
            let id = workspace_id(i);
            assert_eq!(id.len(), 8, "{id}");
            assert!(
                u32::from_str_radix(&id, 16).unwrap() >= 0x1000_0000,
                "{id} is inside the compositor's own range"
            );
        }
    }

    #[test]
    fn a_quote_in_a_name_cannot_break_out_of_the_ron() {
        let ron = render(&[ws(r#"1, name:say "hi""#)], false);
        assert!(ron.contains(r#"name: Some("say \"hi\"")"#), "{ron}");
    }
}
