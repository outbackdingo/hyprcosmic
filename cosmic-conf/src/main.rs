//! `cosmic-conf` — compile a Hyprland-idiom config file into cosmic-config.
//!
//! Exit codes: 0 success, 1 config error (nothing written), 2 usage error.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use cosmic_conf::{assets, emit::Emitter, import, render_diagnostic, watch};

const USAGE: &str = "\
cosmic-conf — compile cosmic.conf into the cosmic-config tree

USAGE:
    cosmic-conf apply [--diff] [--config <path>]
    cosmic-conf import-theme <hypr.theme> [--out <path>] [--report]
                             [--assets [--source <dir>] [--overwrite] [--dry-run]]

OPTIONS:
    --diff            Show what would change without writing anything
    --config <path>   Config file (default: $XDG_CONFIG_HOME/hyprcosmic/cosmic.conf)
    --out <path>      Write the generated cosmic.conf here (default: stdout)
    --report          Print everything that did not translate cleanly
    --assets          Also install wallpapers, GTK/icon themes and the
                      waybar/rofi/kitty theme files that sit beside hypr.theme
    --source <dir>    The theme repo's Source/ directory holding the GTK and
                      icon tarballs (default: found by searching upward)
    --overwrite       Replace assets that are already installed
    --dry-run         With --assets, list what would be installed and stop
    -h, --help        Show this help
";

/// HyDE keeps GTK and icon tarballs in a `Source/` directory at the root of
/// the theme repo, four levels above the theme folder
/// (`Configs/.config/hyde/themes/<Name>/`). Searching upward rather than
/// hardcoding that depth means a theme unpacked at a different depth, or one
/// vendored into another tree, still works.
fn find_source_dir(theme_dir: &std::path::Path) -> Option<PathBuf> {
    theme_dir
        .ancestors()
        .take(6)
        .map(|a| a.join("Source"))
        .find(|c| c.is_dir())
}

/// Refuse arguments the caller does not understand.
///
/// `apply` writes to the config tree, so an argument it does not recognise has
/// to stop it rather than be skipped: `--diff-only` instead of `--diff`, or a
/// config path given positionally, would otherwise apply to the *default*
/// config while looking like it had done what was asked. This is a
/// hand-rolled check rather than a dependency because the whole surface is six
/// flags, and an argument parser that silently ignores the unknown is exactly
/// the behaviour being removed.
///
/// `flags` take no value; `valued` consume the argument after them.
fn reject_unknown(args: &[String], flags: &[&str], valued: &[&str]) -> Result<(), String> {
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if valued.contains(&a.as_str()) {
            i += 2;
        } else if flags.contains(&a.as_str()) {
            i += 1;
        } else if a == "-h" || a == "--help" {
            i += 1;
        } else if let Some(name) = a.strip_prefix("--") {
            return Err(format!("error: unknown option `--{name}`"));
        } else {
            return Err(format!("error: unexpected argument `{a}`"));
        }
    }
    Ok(())
}

fn default_config_path() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(x) if !x.is_empty() => PathBuf::from(x),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
    };
    Some(base.join("hyprcosmic").join("cosmic.conf"))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if args[0] == "import-theme" {
        return match run_import(&args[1..]) {
            Ok(msg) => {
                print!("{msg}");
                ExitCode::SUCCESS
            }
            Err(msg) => {
                eprint!("{msg}");
                ExitCode::from(1)
            }
        };
    }
    if args[0] != "apply" {
        eprintln!("error: unknown command `{}`\n\n{USAGE}", args[0]);
        return ExitCode::from(2);
    }

    if let Err(msg) = reject_unknown(&args[1..], &["--diff"], &["--config"]) {
        eprintln!("{msg}\n\n{USAGE}");
        return ExitCode::from(2);
    }

    let diff_only = args.iter().any(|a| a == "--diff");
    let config_path = match args.iter().position(|a| a == "--config") {
        Some(i) => match args.get(i + 1) {
            Some(p) => PathBuf::from(p),
            None => {
                eprintln!("error: --config needs a path");
                return ExitCode::from(2);
            }
        },
        None => match default_config_path() {
            Some(p) => p,
            None => {
                eprintln!("error: cannot determine config path (no HOME or XDG_CONFIG_HOME)");
                return ExitCode::from(2);
            }
        },
    };

    match run(&config_path, diff_only) {
        Ok(msg) => {
            println!("{msg}");
            ExitCode::SUCCESS
        }
        Err(msg) => {
            eprint!("{msg}");
            ExitCode::from(1)
        }
    }
}

fn run(config_path: &PathBuf, diff_only: bool) -> Result<String, String> {
    let emitter = Emitter::from_env().map_err(|e| format!("error: {e}\n"))?;

    // Through `watch::compile` rather than parse/resolve/plan inline, because
    // that is the only path that expands `source`. Doing it by hand here meant
    // `resolve` never saw the included text -- `flatten` drops `Item::Source`
    // -- so a sourced file was silently ignored by `apply` while `watch`
    // honoured it. An include that works in one and vanishes in the other is
    // worse than one that is unsupported in both.
    let compiled = watch::compile(config_path, &emitter).map_err(|e| e.to_string())?;
    let planned = compiled.planned;

    let changes: Vec<_> = planned.iter().filter(|p| !p.is_noop()).collect();

    if diff_only {
        if changes.is_empty() {
            return Ok("No changes.".into());
        }
        let mut out = String::new();
        for p in &changes {
            let rel = p
                .path
                .strip_prefix(emitter.root())
                .unwrap_or(&p.path)
                .display();
            out.push_str(&format!("~ {rel}\n"));
            match &p.previous {
                Some(prev) => out.push_str(&format!("  - {}\n", prev.trim())),
                None => out.push_str("  - (unset)\n"),
            }
            out.push_str(&format!("  + {}\n", p.contents.trim()));
        }
        out.push_str(&format!("\n{} file(s) would change.", changes.len()));
        return Ok(out);
    }

    let written = emitter.apply(&planned).map_err(|e| format!("error: {e}\n"))?;
    Ok(format!(
        "Applied {written} change(s) to {}.",
        emitter.root().display()
    ))
}

fn run_import(args: &[String]) -> Result<String, String> {
    let Some(src_path) = args.first().filter(|a| !a.starts_with("--")) else {
        return Err(format!("error: import-theme needs a path\n\n{USAGE}"));
    };
    reject_unknown(
        &args[1..],
        &["--report", "--assets", "--overwrite", "--dry-run"],
        &["--out", "--source"],
    )
    .map_err(|msg| format!("{msg}\n\n{USAGE}"))?;
    let out_path = args
        .iter()
        .position(|a| a == "--out")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);
    let want_report = args.iter().any(|a| a == "--report");

    let source = std::fs::read_to_string(src_path)
        .map_err(|e| format!("error: cannot read {src_path}: {e}\n"))?;

    // HyDE names a theme by its containing directory.
    let name = PathBuf::from(src_path)
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "imported".into());

    let imported = import::import_hypr_theme(&source, &name)
        .map_err(|e| render_diagnostic(&source, e.span, &e.message, None))?;

    let mut out = String::new();
    match out_path {
        Some(p) => {
            if let Some(dir) = p.parent() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| format!("error: cannot create {}: {e}\n", dir.display()))?;
            }
            std::fs::write(&p, &imported.conf)
                .map_err(|e| format!("error: cannot write {}: {e}\n", p.display()))?;
            out.push_str(&format!("Wrote {}\n", p.display()));
            if let Some(hint) = unsourced_hint(&p) {
                out.push_str(&hint);
            }
        }
        None => out.push_str(&imported.conf),
    }

    let dropped = imported.dropped().count();
    if want_report {
        out.push('\n');
        out.push_str(&import::render_report(&imported));
    } else if dropped > 0 {
        out.push_str(&format!(
            "\n{dropped} setting(s) did not translate. Re-run with --report for details.\n"
        ));
    }

    if args.iter().any(|a| a == "--assets") {
        out.push('\n');
        out.push_str(&install_assets(src_path, &name, args)?);
    }

    Ok(out)
}

/// Warn when the file just written is not reachable from its sibling
/// cosmic.conf, and say what to add.
///
/// A theme lives in its own file so that re-importing cannot clobber the
/// keybindings around it, but that only works if something sources it.
/// Writing an inert file and reporting success is the worst of both: the tool
/// looks like it worked and the desktop does not change.
///
/// The match is textual and deliberately loose -- it is looking for evidence
/// that the user already knows about the file, not parsing the config. A false
/// negative costs one redundant hint; a false positive would hide a real
/// problem, so the substring searched for is the filename itself.
fn unsourced_hint(written: &Path) -> Option<String> {
    let dir = written.parent()?;
    let name = written.file_name()?.to_string_lossy().to_string();
    let main = dir.join("cosmic.conf");

    // Nothing to say when the theme *is* the config, or there is no config yet
    // to add a line to: `apply` will be pointed at this file directly.
    if main == written || !main.exists() {
        return None;
    }

    let text = std::fs::read_to_string(&main).ok()?;
    if text
        .lines()
        .any(|l| l.trim_start().starts_with("source") && l.contains(&name))
    {
        return None;
    }

    Some(format!(
        "\nNothing sources it yet, so `apply` will ignore it. Add this to {}:\n\n    source = {}\n",
        main.display(),
        written.display(),
    ))
}

/// The half of a theme that is not config: wallpapers, GTK/icon tarballs, and
/// the `.theme` files belonging to waybar, rofi and kitty.
///
/// Separate from the conf translation because it is separate in kind — none of
/// it is translated, only placed — and because it writes outside the
/// cosmic-config tree, which every other path in this tool does not.
fn install_assets(src_path: &str, name: &str, args: &[String]) -> Result<String, String> {
    let theme_dir = PathBuf::from(src_path)
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("error: {src_path} has no parent directory\n"))?;

    let source_dir = match args.iter().position(|a| a == "--source") {
        Some(i) => match args.get(i + 1) {
            Some(p) => Some(PathBuf::from(p)),
            None => return Err(format!("error: --source needs a path\n\n{USAGE}")),
        },
        None => find_source_dir(&theme_dir),
    };

    let installer = assets::Installer::from_env().map_err(|e| format!("error: {e}\n"))?;
    let plan = installer
        .plan(
            &theme_dir,
            source_dir.as_deref(),
            name,
            args.iter().any(|a| a == "--overwrite"),
        )
        .map_err(|errors| {
            errors
                .iter()
                .map(|e| format!("error: {e}\n"))
                .collect::<String>()
        })?;

    if args.iter().any(|a| a == "--dry-run") {
        return Ok(assets::render_plan(&plan));
    }

    let report = installer
        .apply(&plan)
        .map_err(|e| format!("error: {e}\n"))?;
    Ok(assets::render_report(&plan, &report))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn known_flags_and_their_values_are_accepted() {
        let a = args(&["--diff", "--config", "/etc/cosmic.conf"]);
        assert!(reject_unknown(&a, &["--diff"], &["--config"]).is_ok());
    }

    #[test]
    fn a_misspelt_flag_is_refused_rather_than_skipped() {
        // The bug this exists to prevent: `--diff-only` used to be ignored, so
        // `apply` wrote for real while the caller believed it was a dry run.
        let a = args(&["--diff-only"]);
        let err = reject_unknown(&a, &["--diff"], &["--config"]).unwrap_err();
        assert!(err.contains("--diff-only"), "{err}");
    }

    #[test]
    fn a_positional_path_is_refused_because_it_would_be_ignored() {
        let a = args(&["/home/me/.config/hyprcosmic/cosmic.conf"]);
        let err = reject_unknown(&a, &["--diff"], &["--config"]).unwrap_err();
        assert!(err.contains("unexpected argument"), "{err}");
    }

    #[test]
    fn a_value_that_looks_like_a_flag_is_still_a_value() {
        // `--config --diff` is a user error, but it is the *next* argument's
        // job to be a path; consuming it here keeps the rule simple and
        // matches what the position-based lookup below actually does.
        let a = args(&["--config", "--diff"]);
        assert!(reject_unknown(&a, &["--diff"], &["--config"]).is_ok());
    }

    #[test]
    fn a_trailing_valued_flag_with_no_value_is_not_a_panic() {
        let a = args(&["--config"]);
        assert!(reject_unknown(&a, &["--diff"], &["--config"]).is_ok());
    }
}
