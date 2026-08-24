//! crap4rust — CRAP metric for Rust cargo workspaces.
//!
//! Thin entrypoint: it owns only the process concerns (argument parsing hand-off
//! and exit codes) and delegates all work to the library's `cli` module.
//!
//! **Exit codes (C6 — crap4go parity).** `0` on success, *including* when
//! high-CRAP functions are reported and when coverage paths resolve only
//! fuzzily or not at all; `1` on operational error only. Errors and coverage
//! resolution diagnostics (FC-T5b) go to stderr, the report to stdout. Clap's
//! own default of exit 2 for bad arguments is deliberately overridden via
//! `try_parse`, and the error is rendered explicitly rather than relying on
//! `Termination`'s `Debug` formatting.

use std::process::ExitCode;

use clap::Parser;
use crap4rust::cli::{self, Cli, RunConfig};

/// Operational-error exit code (C6).
const FAILURE: u8 = 1;

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
    // Emitted before the run so a coverage command that will never run, or one
    // that must write LCOV itself, is flagged up front rather than after a long
    // test run. Advisory only — never an exit-code effect (C6).
    for advisory in config.advisories() {
        eprintln!("{advisory}");
    }

    match cli::run(&config) {
        Ok(output) => {
            // Advisory only — diagnostics never change the exit code (C6).
            for diagnostic in &output.diagnostics {
                eprintln!("{diagnostic}");
            }
            // `format_report` is fully newline-terminated (FC-T6e).
            let report = output.report;
            print!("{report}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::from(FAILURE)
        }
    }
}
