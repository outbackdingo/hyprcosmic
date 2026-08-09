//! `cosmic-conf` — compile a Hyprland-idiom config file into cosmic-config.
//!
//! Exit codes: 0 success, 1 config error (nothing written), 2 usage error.

use std::path::PathBuf;
use std::process::ExitCode;

use cosmic_conf::{emit::Emitter, parse, render_diagnostic, resolve};

const USAGE: &str = "\
cosmic-conf — compile cosmic.conf into the cosmic-config tree

USAGE:
    cosmic-conf apply [--diff] [--config <path>]

OPTIONS:
    --diff            Show what would change without writing anything
    --config <path>   Config file (default: $XDG_CONFIG_HOME/hyprcosmic/cosmic.conf)
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
