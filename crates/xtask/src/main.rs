//! Repo automation.
//!
//! `xtask gates` renders `docs/GATES.md` from the single source of truth
//! ([`holonomy_ledger::laws::LAWS`]); `xtask gates --check` exits 1 if the checked-in file
//! differs from the render, so the human-readable contract can never drift from the code.
//! CI runs `--check` on every commit.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use holonomy_ledger::laws::LAWS;

const USAGE: &str = "usage: cargo run -p xtask -- gates [--check]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["gates"] => match write_gates() {
            Ok(path) => {
                println!("wrote {}", path.display());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("xtask gates: {e}");
                ExitCode::FAILURE
            }
        },
        ["gates", "--check"] => match check_gates() {
            Ok(()) => {
                println!("docs/GATES.md is in sync with holonomy-ledger LAWS");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("xtask gates --check: {e}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// Render the GATES contract from the law table. The format is deliberately boring: a fixed
/// header, one table, no timestamps — so `git diff` and `--check` are both exact.
fn render_gates() -> String {
    let mut out = String::from(
        "# HOLONOMY v6 \u{2014} GATES\n\n\
         > Machine-generated contract. Source of truth: `LAWS` in\n\
         > `crates/holonomy-ledger/src/laws.rs`. Regenerate: `cargo run -p xtask -- gates`.\n\
         > Verify: `cargo run -p xtask -- gates --check`. Do not edit by hand.\n\n\
         | ID | Law | Bound | Enforcing block | Status |\n\
         | -- | --- | ----- | --------------- | ------ |\n",
    );
    for law in LAWS {
        writeln!(
            out,
            "| {} | {} | {} | {} | {} |",
            law.id,
            law.name,
            law.bound,
            law.block,
            law.status.token()
        )
        .expect("writing to a String cannot fail");
    }
    out.push('\n');
    out
}

/// Repository root = two levels above this crate's manifest directory.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn gates_path() -> PathBuf {
    repo_root().join("docs").join("GATES.md")
}

fn write_gates() -> Result<PathBuf, String> {
    let path = gates_path();
    std::fs::write(&path, render_gates())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path)
}

fn check_gates() -> Result<(), String> {
    let path = gates_path();
    let on_disk = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let generated = render_gates();
    // Trailing-whitespace tolerance, so handwritten file endings cannot produce false drift.
    if on_disk.trim_end() == generated.trim_end() {
        Ok(())
    } else {
        Err(format!(
            "{} is stale or hand-edited. Run `cargo run -p xtask -- gates` and commit the result.",
            path.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_contains_every_law_id() {
        let rendered = render_gates();
        for law in LAWS {
            assert!(
                rendered.contains(&format!("| {} | {}", law.id, law.name)),
                "render missing {}",
                law.id
            );
        }
    }
}
