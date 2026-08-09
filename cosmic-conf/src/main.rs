//! `cosmic-conf` — compile a Hyprland-idiom config file into cosmic-config.
//!
//! Exit codes: 0 success, 1 config error (nothing written), 2 usage error.

use std::path::PathBuf;
use std::process::ExitCode;

use cosmic_conf::{emit::Emitter, import, parse, render_diagnostic, resolve};

const USAGE: &str = "\
cosmic-conf — compile cosmic.conf into the cosmic-config tree

USAGE:
    cosmic-conf apply [--diff] [--config <path>]
    cosmic-conf import-theme <hypr.theme> [--out <path>] [--report]

OPTIONS:
    --diff            Show what would change without writing anything
    --config <path>   Config file (default: $XDG_CONFIG_HOME/hyprcosmic/cosmic.conf)
    --out <path>      Write the generated cosmic.conf here (default: stdout)
    --report          Print everything that did not translate cleanly
    -h, --help        Show this help
";

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
    let source = std::fs::read_to_string(config_path)
        .map_err(|e| format!("error: cannot read {}: {e}\n", config_path.display()))?;

    let ast = parse(&source).map_err(|e| {
        render_diagnostic(&source, e.span, &e.message, None)
    })?;

    let resolved = resolve(&ast).map_err(|diags| {
        let mut out = String::new();
        for d in &diags {
            out.push_str(&render_diagnostic(&source, d.span, &d.message, d.help.as_deref()));
            out.push('\n');
        }
        out.push_str(&format!(
            "error: {} problem(s) found; nothing was written\n",
            diags.len()
        ));
        out
    })?;

    let emitter = Emitter::from_env().map_err(|e| format!("error: {e}\n"))?;
    let planned = emitter.plan(&resolved).map_err(|errs| {
        let mut out = String::new();
        for e in &errs {
            out.push_str(&format!("error: {e}\n"));
        }
        out.push_str("error: nothing was written\n");
        out
    })?;

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
    Ok(out)
}
