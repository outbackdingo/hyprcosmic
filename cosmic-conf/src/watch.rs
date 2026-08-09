//! Filesystem watch: re-apply `cosmic.conf` on every edit.
//!
//! Two problems make this more than "call `notify` and re-run `main`'s
//! pipeline":
//!
//! 1. **`source` fans out the watch set.** `parser::Item::Source` lets a
//!    config pull in other files (`parser.rs:66`), but `resolve` treats
//!    `Source` as inert (`resolve.rs:87`) — nothing upstream actually expands
//!    it yet, even though `resolve.rs:66` already assumes an "include
//!    expansion" pass ran first. This module is that pass: `merge_text`
//!    textually splices a sourced file's contents in place of its `source`
//!    line, the same way Hyprland treats `source` as literal inclusion. Doing
//!    it as text rather than AST-splicing means the merged string is one
//!    coherent document, so `Span`s (which are just line/col, with no file
//!    identity — `parser.rs:24`) stay correct for `render_diagnostic`
//!    regardless of which physical file a line came from. It also means the
//!    watch set has to be recomputed after every successful compile, since
//!    editing a `source` line can add or remove files from it.
//!
//! 2. **A bad edit must not kill the daemon or half-apply.** `Emitter::plan`
//!    already keeps `apply` transactional (`emit.rs:11-14`); this module's
//!    job is to keep that guarantee across an unbounded stream of edits by
//!    treating every compile failure as "log it and keep watching" rather
//!    than propagating it out of the loop.
//!
//! The event loop itself (`watch`) is intentionally thin. Everything with
//! interesting logic — merging sources, debouncing — is a free function
//! usable without a real inotify watcher, per the module's tests.

use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

use crate::emit::{EmitError, Emitter, Planned};
use crate::parser::{self, Item, ParseError};
use crate::render_diagnostic;
use crate::resolve::{self, Diagnostic};

/// Editors commonly write a save as several syscalls (truncate, write,
/// rename); this is long enough to collapse those into one recompile without
/// making a real edit feel laggy.
const DEBOUNCE: Duration = Duration::from_millis(250);

/// Everything that can go wrong compiling `config` (and whatever it sources)
/// into a plan. Every variant renders a complete, human-readable report —
/// `watch` just prints `Display` and moves on.
#[derive(Debug)]
pub enum CompileError {
    /// `config`, or something it `source`s, could not be read.
    Read { path: PathBuf, error: io::Error },
    /// A `source` chain refers back to a file already being expanded.
    /// Splicing it would recurse forever, so this is reported instead.
    Cycle { path: PathBuf },
    /// A single file failed to parse on its own, before merging — `source`
    /// and `error` are both that file's, so the line number is exact.
    Parse {
        path: PathBuf,
        source: String,
        error: ParseError,
    },
    /// The merged document failed to resolve. `source` is the full merged
    /// text, so `diagnostics`' spans point at the right physical line no
    /// matter which file contributed it.
    Resolve {
        source: String,
        diagnostics: Vec<Diagnostic>,
    },
    /// Resolved cleanly but could not be turned into file contents.
    Emit(Vec<EmitError>),
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CompileError::Read { path, error } => {
                write!(f, "error: cannot read {}: {error}\n", path.display())
            }
            CompileError::Cycle { path } => {
                write!(
                    f,
                    "error: `source` cycle detected while expanding {}\n",
                    path.display()
                )
            }
            CompileError::Parse { path, source, error } => {
                write!(
                    f,
                    "in {}:\n{}",
                    path.display(),
                    render_diagnostic(source, error.span, &error.message, None)
                )
            }
            CompileError::Resolve { source, diagnostics } => {
                let mut out = String::new();
                for d in diagnostics {
                    out.push_str(&render_diagnostic(source, d.span, &d.message, d.help.as_deref()));
                    out.push('\n');
                }
                out.push_str(&format!(
                    "error: {} problem(s) found; nothing was written\n",
                    diagnostics.len()
                ));
                write!(f, "{out}")
            }
            CompileError::Emit(errs) => {
                let mut out = String::new();
                for e in errs {
                    out.push_str(&format!("error: {e}\n"));
                }
                out.push_str("error: nothing was written\n");
                write!(f, "{out}")
            }
        }
    }
}

impl std::error::Error for CompileError {}

/// A completed compile: what to write, and what to watch.
#[derive(Debug)]
pub struct Compiled {
    pub planned: Vec<Planned>,
    /// Every file that contributed content, `config` first. This is exactly
    /// the set `watch` needs to be subscribed to for the *next* edit to be
    /// noticed, and it can change from one compile to the next as `source`
    /// lines are added, removed, or edited.
    pub sources: Vec<PathBuf>,
}

/// Parse `config` — following `source` directives — resolve, and plan writes
/// against `emitter`, without touching disk.
///
/// Pulled out of `watch` so the compile pipeline is unit-testable without a
/// filesystem watcher: every test in this module drives `compile` directly.
pub fn compile(config: &Path, emitter: &Emitter) -> Result<Compiled, CompileError> {
    let mut ancestors = Vec::new();
    let mut sources = Vec::new();
    let merged = merge_text(config, &mut ancestors, &mut sources)?;

    let ast = parser::parse(&merged).map_err(|error| CompileError::Parse {
        path: config.to_path_buf(),
        source: merged.clone(),
        error,
    })?;

    let resolved = resolve::resolve(&ast).map_err(|diagnostics| CompileError::Resolve {
        source: merged,
        diagnostics,
    })?;

    let planned = emitter.plan(&resolved).map_err(CompileError::Emit)?;

    Ok(Compiled { planned, sources })
}

/// Read `path`, then replace every `source = <path>` line with the
/// (recursively expanded) text of the sourced file, so the result is one
/// document `parser::parse` can consume in a single pass — see the module
/// doc for why textual splicing rather than AST splicing.
///
/// `ancestors` is the current inclusion chain (for cycle detection);
/// `watched` accumulates every file visited, in the order first seen.
fn merge_text(
    path: &Path,
    ancestors: &mut Vec<PathBuf>,
    watched: &mut Vec<PathBuf>,
) -> Result<String, CompileError> {
    let key = path.to_path_buf();
    if ancestors.contains(&key) {
        return Err(CompileError::Cycle { path: key });
    }

    let raw = fs::read_to_string(path).map_err(|error| CompileError::Read {
        path: key.clone(),
        error,
    })?;
    watched.push(key.clone());

    // Parsing here (rather than scanning text for `source =` ourselves) means
    // we inherit the grammar's exact rules for comments and whitespace, so
    // the line we splice at is always the one the real parser would call a
    // `Source` item.
    let ast = parser::parse(&raw).map_err(|error| CompileError::Parse {
        path: key.clone(),
        source: raw.clone(),
        error,
    })?;

    let mut targets = Vec::new();
    collect_sources(&ast.items, &mut targets);
    if targets.is_empty() {
        return Ok(raw);
    }

    // Splicing changes line counts, so process bottom-up: replacing a later
    // line first leaves every earlier line number still valid.
    targets.sort_by(|a, b| b.0.cmp(&a.0));

    ancestors.push(key);
    let mut lines: Vec<String> = raw.lines().map(String::from).collect();
    for (line_no, raw_path) in targets {
        let target_path = resolve_source_path(path, &raw_path);
        let included = merge_text(&target_path, ancestors, watched)?;
        lines.splice(line_no - 1..line_no, included.lines().map(String::from));
    }
    ancestors.pop();

    let mut out = lines.join("\n");
    out.push('\n');
    Ok(out)
}

/// Depth-first walk collecting every `source` item's `(line, raw path)`.
/// Sections are recursed into: a `source` nested inside `general { .. }`
/// splices its contents into that section, matching Hyprland's textual
/// `source` semantics rather than only supporting top-level includes.
fn collect_sources(items: &[Item], out: &mut Vec<(usize, String)>) {
    for item in items {
        match item {
            Item::Source { path } => out.push((path.span.line, path.value.clone())),
            Item::Section { items, .. } => collect_sources(items, out),
            _ => {}
        }
    }
}

/// Resolve a `source` value the way a shell prompt would: `~/` against
/// `$HOME`, everything else relative to the directory of the file doing the
/// sourcing (not the process's cwd), so a config tree keeps working wherever
/// it is checked out.
fn resolve_source_path(containing_file: &Path, raw: &str) -> PathBuf {
    let expanded = if raw == "~" {
        std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(raw))
    } else if let Some(rest) = raw.strip_prefix("~/") {
        match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join(rest),
            None => PathBuf::from(raw),
        }
    } else {
        PathBuf::from(raw)
    };

    if expanded.is_absolute() {
        expanded
    } else {
        containing_file
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(expanded)
    }
}

/// Block for the first item on `rx`, then keep draining anything that
/// arrives within `window` of the previous one. Returns `None` once the
/// sender side has been dropped and nothing more will ever come.
///
/// This is the whole debounce policy, factored out of `watch`'s loop so it
/// can be tested against a plain channel instead of real filesystem events —
/// editors write a save as several syscalls, and without this a single save
/// would trigger several redundant recompiles.
fn collect_batch<T>(rx: &mpsc::Receiver<T>, window: Duration) -> Option<Vec<T>> {
    let first = rx.recv().ok()?;
    let mut batch = vec![first];
    while let Ok(next) = rx.recv_timeout(window) {
        batch.push(next);
    }
    Some(batch)
}

/// Bring `watcher`'s subscriptions in line with `wanted`, diffing against
/// `current` so files that stopped being sourced are actually unwatched
/// (otherwise the watch set only ever grows).
///
/// Best-effort: a `watch`/`unwatch` failure (e.g. a sourced file that does
/// not exist yet) is not fatal — the next successful compile will retry with
/// whatever the config asks for at that point.
fn sync_watches(watcher: &mut RecommendedWatcher, current: &mut HashSet<PathBuf>, wanted: &[PathBuf]) {
    let wanted: HashSet<PathBuf> = wanted.iter().cloned().collect();

    for stale in current.difference(&wanted) {
        let _ = watcher.unwatch(stale);
    }
    for fresh in wanted.difference(current) {
        let _ = watcher.watch(fresh, RecursiveMode::NonRecursive);
    }

    *current = wanted;
}

/// Anything that stops the daemon outright. Deliberately small: a broken
/// `cosmic.conf` is *not* one of these — see the module doc — so this is
/// only the notify plumbing itself failing to start.
#[derive(Debug)]
pub enum WatchError {
    Notify(notify::Error),
}

impl fmt::Display for WatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WatchError::Notify(e) => write!(f, "watch error: {e}"),
        }
    }
}

impl std::error::Error for WatchError {}

impl From<notify::Error> for WatchError {
    fn from(e: notify::Error) -> Self {
        WatchError::Notify(e)
    }
}

/// Watch `config` — and everything it currently `source`s — reapplying on
/// every change until the watcher itself fails to start or stops delivering
/// events. A malformed edit is reported to stderr and waited past: see the
/// module doc for why that, not propagating the error, is the contract here.
pub fn watch(config: &Path, emitter: &Emitter) -> Result<(), WatchError> {
    let (tx, rx) = mpsc::channel::<notify::Result<Event>>();
    let mut watcher: RecommendedWatcher = notify::recommended_watcher(tx)?;
    let mut watched: HashSet<PathBuf> = HashSet::new();

    // Compile once up front: the desktop should reflect the config the
    // moment the daemon starts, and this also tells us the initial watch
    // set. If it fails, fall back to watching just `config` — that is the
    // one file guaranteed to exist, and a later successful compile will
    // widen the watch set to whatever it actually sources.
    match compile(config, emitter) {
        Ok(compiled) => {
            if let Err(e) = emitter.apply(&compiled.planned) {
                eprintln!("{}", CompileError::Emit(vec![e]));
            }
            sync_watches(&mut watcher, &mut watched, &compiled.sources);
        }
        Err(e) => {
            eprintln!("{e}");
            sync_watches(&mut watcher, &mut watched, std::slice::from_ref(&config.to_path_buf()));
        }
    }

    loop {
        let Some(batch) = collect_batch(&rx, DEBOUNCE) else {
            // The sender was dropped, which only happens if `watcher` itself
            // was torn down — nothing more will ever arrive.
            return Ok(());
        };

        for event in &batch {
            if let Err(e) = event {
                eprintln!("watch error: {e}");
            }
        }

        match compile(config, emitter) {
            Ok(compiled) => {
                if let Err(e) = emitter.apply(&compiled.planned) {
                    eprintln!("{}", CompileError::Emit(vec![e]));
                }
                sync_watches(&mut watcher, &mut watched, &compiled.sources);
            }
            Err(e) => {
                // Leave `watched` alone: the fix for a bad edit might land in
                // an already-sourced file, and dropping back to watching
                // only `config` would miss that.
                eprintln!("{e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, contents).unwrap();
        path
    }

    // ---- compile ----------------------------------------------------

    #[test]
    fn compile_with_no_source_directives_watches_just_the_config() {
        let conf_dir = TempDir::new().unwrap();
        let root_dir = TempDir::new().unwrap();
        let config = write(conf_dir.path(), "cosmic.conf", "general {\n  autotile = true\n}\n");

        let compiled = compile(&config, &Emitter::with_root(root_dir.path())).unwrap();

        assert_eq!(compiled.sources, vec![config]);
        assert_eq!(compiled.planned.len(), 1);
    }

    #[test]
    fn compile_follows_a_source_directive_and_lists_it_as_a_watch_target() {
        let conf_dir = TempDir::new().unwrap();
        let root_dir = TempDir::new().unwrap();
        let included = write(conf_dir.path(), "extra.conf", "general {\n  autotile = true\n}\n");
        let config = write(conf_dir.path(), "cosmic.conf", "source = extra.conf\n");

        let compiled = compile(&config, &Emitter::with_root(root_dir.path())).unwrap();

        assert_eq!(compiled.sources, vec![config, included]);
        assert_eq!(compiled.planned.len(), 1);
    }

    #[test]
    fn compile_expands_a_source_nested_inside_a_section() {
        // The sourced file's contents become part of the enclosing section,
        // the same way Hyprland's `source` is a literal text substitution.
        let conf_dir = TempDir::new().unwrap();
        let root_dir = TempDir::new().unwrap();
        write(conf_dir.path(), "gaps.conf", "gaps_in = 5\ngaps_out = 10\n");
        let config = write(
            conf_dir.path(),
            "cosmic.conf",
            "general {\n  source = gaps.conf\n  autotile = true\n}\n",
        );

        let planned = compile(&config, &Emitter::with_root(root_dir.path()))
            .unwrap()
            .planned;
        let gaps = planned.iter().find(|p| p.path.ends_with("gaps")).expect("gaps planned");
        assert_eq!(gaps.contents, "(10, 5)");
    }

    #[test]
    fn compile_resolves_relative_sources_against_the_including_files_directory() {
        // The including file lives in a subdirectory; `nested.conf` must be
        // found relative to it, not relative to the process's cwd.
        let conf_dir = TempDir::new().unwrap();
        let sub = conf_dir.path().join("sub");
        fs::create_dir_all(&sub).unwrap();
        write(&sub, "nested.conf", "general {\n  autotile = true\n}\n");
        let config = write(&sub, "cosmic.conf", "source = nested.conf\n");

        let root_dir = TempDir::new().unwrap();
        let compiled = compile(&config, &Emitter::with_root(root_dir.path())).unwrap();
        assert_eq!(compiled.sources.len(), 2);
    }

    #[test]
    fn compile_expands_tilde_against_home() {
        let home = TempDir::new().unwrap();
        write(home.path(), "shared.conf", "general {\n  autotile = true\n}\n");
        let conf_dir = TempDir::new().unwrap();
        let config = write(conf_dir.path(), "cosmic.conf", "source = ~/shared.conf\n");

        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());
        let result = compile(&config, &Emitter::with_root(TempDir::new().unwrap().path()));
        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }

        let compiled = result.unwrap();
        assert!(compiled.sources.contains(&home.path().join("shared.conf")));
    }

    #[test]
    fn compile_follows_a_chain_of_nested_sources() {
        let conf_dir = TempDir::new().unwrap();
        write(conf_dir.path(), "c.conf", "autotile = true\n");
        write(conf_dir.path(), "b.conf", "general {\n  source = c.conf\n}\n");
        let config = write(conf_dir.path(), "a.conf", "source = b.conf\n");

        let compiled = compile(&config, &Emitter::with_root(TempDir::new().unwrap().path())).unwrap();
        assert_eq!(compiled.sources.len(), 3);
        assert_eq!(compiled.planned.len(), 1);
    }

    #[test]
    fn compile_detects_a_source_cycle() {
        let conf_dir = TempDir::new().unwrap();
        let a = conf_dir.path().join("a.conf");
        let b = conf_dir.path().join("b.conf");
        fs::write(&a, "source = b.conf\n").unwrap();
        fs::write(&b, "source = a.conf\n").unwrap();

        let err = compile(&a, &Emitter::with_root(TempDir::new().unwrap().path())).unwrap_err();
        assert!(matches!(err, CompileError::Cycle { .. }), "{err}");
    }

    #[test]
    fn compile_reports_a_missing_source_file_without_panicking() {
        let conf_dir = TempDir::new().unwrap();
        let config = write(conf_dir.path(), "cosmic.conf", "source = does-not-exist.conf\n");

        let err = compile(&config, &Emitter::with_root(TempDir::new().unwrap().path())).unwrap_err();
        assert!(matches!(err, CompileError::Read { .. }), "{err}");
    }

    #[test]
    fn compile_surfaces_a_syntax_error_in_a_sourced_file() {
        let conf_dir = TempDir::new().unwrap();
        write(conf_dir.path(), "broken.conf", "this is not valid\n");
        let config = write(conf_dir.path(), "cosmic.conf", "source = broken.conf\n");

        let err = compile(&config, &Emitter::with_root(TempDir::new().unwrap().path())).unwrap_err();
        match err {
            CompileError::Parse { path, .. } => assert_eq!(path, conf_dir.path().join("broken.conf")),
            other => panic!("expected Parse, got {other}"),
        }
    }

    #[test]
    fn compile_diagnostic_line_number_points_at_the_merged_document_not_the_fragment() {
        // The offending line is line 1 of `bad.conf`, but after splicing it
        // sits at line 2 of the merged document — the diagnostic must report
        // the merged position so the caret lands on the right physical line.
        let conf_dir = TempDir::new().unwrap();
        write(conf_dir.path(), "bad.conf", "gaps_inn = 8\n");
        let config = write(
            conf_dir.path(),
            "cosmic.conf",
            "general {\n  source = bad.conf\n}\n",
        );

        let err = compile(&config, &Emitter::with_root(TempDir::new().unwrap().path())).unwrap_err();
        match err {
            CompileError::Resolve { diagnostics, .. } => {
                assert_eq!(diagnostics[0].span.line, 2);
            }
            other => panic!("expected Resolve, got {other}"),
        }
    }

    #[test]
    fn compile_surfaces_resolve_diagnostics_for_an_unknown_key() {
        let conf_dir = TempDir::new().unwrap();
        let config = write(conf_dir.path(), "cosmic.conf", "general {\n  gaps_inn = 8\n}\n");

        let err = compile(&config, &Emitter::with_root(TempDir::new().unwrap().path())).unwrap_err();
        assert!(matches!(err, CompileError::Resolve { .. }), "{err}");
        assert!(err.to_string().contains("unknown key"), "{err}");
    }

    /// Mirrors `emit.rs`'s `plan_does_not_write`: `compile` only plans, so it
    /// must leave the cosmic-config tree untouched.
    #[test]
    fn compile_does_not_write_to_the_config_root() {
        let conf_dir = TempDir::new().unwrap();
        let config = write(conf_dir.path(), "cosmic.conf", "general {\n  autotile = true\n}\n");
        let root_dir = TempDir::new().unwrap();

        let _ = compile(&config, &Emitter::with_root(root_dir.path())).unwrap();

        assert!(
            fs::read_dir(root_dir.path()).unwrap().next().is_none(),
            "compile must leave the tree untouched"
        );
    }

    // ---- resolve_source_path -----------------------------------------

    #[test]
    fn resolve_source_path_is_relative_to_the_including_file_not_the_cwd() {
        let including = Path::new("/somewhere/deep/cosmic.conf");
        assert_eq!(
            resolve_source_path(including, "extra.conf"),
            Path::new("/somewhere/deep/extra.conf")
        );
    }

    #[test]
    fn resolve_source_path_leaves_absolute_paths_alone() {
        let including = Path::new("/somewhere/deep/cosmic.conf");
        assert_eq!(
            resolve_source_path(including, "/etc/other.conf"),
            Path::new("/etc/other.conf")
        );
    }

    // Tilde expansion against `$HOME` is covered end-to-end by
    // `compile_expands_tilde_against_home` below rather than here too:
    // `std::env::set_var` mutates process-global state, and the default
    // test runner is multi-threaded, so two tests racing to set `HOME`
    // would be a real source of flakiness rather than a hypothetical one.

    // ---- collect_batch (debounce) -------------------------------------
    //
    // These exercise the debounce policy directly against a plain channel,
    // with no filesystem or notify involvement at all, so they are fast and
    // cannot flake on OS-level event timing.

    #[test]
    fn collect_batch_drains_everything_already_sent_before_it_was_called() {
        let (tx, rx) = mpsc::channel();
        for i in 0..5 {
            tx.send(i).unwrap();
        }
        let batch = collect_batch(&rx, Duration::from_millis(30)).unwrap();
        assert_eq!(batch, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn collect_batch_returns_none_once_the_sender_is_dropped() {
        let (tx, rx) = mpsc::channel::<i32>();
        drop(tx);
        assert!(collect_batch(&rx, Duration::from_millis(30)).is_none());
    }

    #[test]
    fn collect_batch_starts_a_fresh_batch_after_the_quiet_window_elapses() {
        use std::thread;

        let (tx, rx) = mpsc::channel();
        let window = Duration::from_millis(20);

        tx.send(1).unwrap();
        let first = collect_batch(&rx, window).unwrap();
        assert_eq!(first, vec![1]);

        // Send the second burst from another thread after the window has
        // safely elapsed (10x margin), so the main thread's blocking `recv`
        // in the next `collect_batch` call has something to wake it up.
        thread::spawn(move || {
            thread::sleep(window * 10);
            tx.send(2).unwrap();
        });
        let second = collect_batch(&rx, window).unwrap();
        assert_eq!(second, vec![2]);
    }

    // ---- sync_watches ---------------------------------------------------
    //
    // Exercises the real notify watch/unwatch bookkeeping — but only ever
    // registers watches on files that already exist; no event is triggered
    // or waited for, so this cannot flake on inotify timing.

    #[test]
    fn sync_watches_adds_and_then_removes_a_watch() {
        let dir = TempDir::new().unwrap();
        let a = write(dir.path(), "a.conf", "");
        let b = write(dir.path(), "b.conf", "");

        let (tx, _rx) = mpsc::channel::<notify::Result<Event>>();
        let mut watcher = notify::recommended_watcher(tx).unwrap();
        let mut current = HashSet::new();

        sync_watches(&mut watcher, &mut current, &[a.clone(), b.clone()]);
        assert_eq!(current, HashSet::from([a.clone(), b.clone()]));

        // Dropping `b` from the wanted set must unwatch it, not just stop
        // tracking it, or the watch set would only ever grow.
        sync_watches(&mut watcher, &mut current, &[a.clone()]);
        assert_eq!(current, HashSet::from([a]));
    }
}
