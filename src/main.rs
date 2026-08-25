//! crap4rust — CRAP metric for Rust cargo workspaces.
//!
//! Thin entrypoint: it owns only the process concerns (argument parsing hand-off
//! and exit codes) and delegates all work to the library's `cli` module.
//!
//! **Exit codes (C6 — crap4go parity).** `0` on success, *including* when
//! high-CRAP functions are reported and when coverage paths resolve only
//! fuzzily or not at all; `1` on operational error only. Errors and
//! diagnostics (FC-T5b/FC-T9d) go to stderr, the report to stdout. Clap's
//! own default of exit 2 for bad arguments is deliberately overridden via
//! `try_parse`, and the error is rendered explicitly rather than relying on
//! `Termination`'s `Debug` formatting.

use std::process::ExitCode;

use clap::Parser;
use crap4rust::cli::{self, Cli, Diagnostic, RunConfig};

/// Operational-error exit code (C6).
const FAILURE: u8 = 1;

/// Print and consume everything the sink has collected so far. Advisory
/// only — diagnostics never change the exit code (C6).
fn drain(diagnostics: &mut Vec<Diagnostic>) {
    for diagnostic in diagnostics.drain(..) {
        eprintln!("{diagnostic}");
    }
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            // Clap routes `--help`/`--version` to stdout and real argument
            // errors to stderr; `use_stderr` distinguishes the two.
            let _ = err.print();
            return if err.use_stderr() {
                ExitCode::from(FAILURE)
            } else {
                ExitCode::SUCCESS
            };
        }
    };

    let config = RunConfig::from(&cli);
    // One sink for everything the invocation has to say (FC-T9d).
    let mut diagnostics: Vec<Diagnostic> = config.diagnostics().to_vec();
    // Config-phase facts are drained before the run so a coverage command that
    // will never run, or one that must write LCOV itself, is flagged up front
    // rather than after a long test run (FC-T8g).
    drain(&mut diagnostics);

    let result = cli::run(&config, &mut diagnostics);
    // Drained on `Err` too: three unresolvable modules explain the parse error
    // that follows them far better than the parse error alone does.
    drain(&mut diagnostics);

    match result {
        Ok(report) => {
            // `format_report` is fully newline-terminated (FC-T6e).
            print!("{report}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::from(FAILURE)
        }
    }
}
