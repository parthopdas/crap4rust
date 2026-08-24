//! CLI edge (T7) — argument parsing and pipeline composition.
//!
//! This is the only place that knows about the filesystem layout of a run: it
//! discovers Rust sources, loads the LCOV profile, and drives the pure core
//! (CC engine → join → reporter). Everything it calls inward is pure.
//!
//! **Process concerns stay in `main.rs`.** [`run`] is fallible and *returns*
//! the rendered report plus any resolution diagnostics instead of printing
//! them, so the whole pipeline is exercisable without capturing process
//! output; `main` maps `Err` to stderr + exit code 1, writes diagnostics to
//! stderr, and prints the report to stdout (C6).
//!
//! **S1 surface only.** `--lcov-path` is required because running coverage is
//! T8/S2; source discovery is a deliberately minimal `.rs` walk because
//! workspace enumeration is T9 and product/test filtering is T10. There is no
//! `--threshold`: v1 is a reporter, not a gate (C6/C11/D1).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use clap::Parser;

use crate::{complexity, coverage, join, report};

/// `crap4rust --lcov-path <PATH> <PATH>` — the S1 walking-skeleton surface.
#[derive(Parser)]
#[command(name = "crap4rust", version, about, long_about = None)]
pub struct Cli {
    /// Path to an LCOV coverage file (as emitted by `cargo llvm-cov --lcov`).
    #[arg(long, value_name = "PATH")]
    lcov_path: PathBuf,

    /// Rust source file, or directory to scan recursively for `.rs` files.
    #[arg(value_name = "PATH")]
    path: PathBuf,
}

/// Everything a run produces for the process to emit: the rendered report and
/// the coverage-resolution diagnostics (FC-T5b), kept apart because they go to
/// different streams — report to stdout, diagnostics to stderr.
pub struct RunOutput {
    /// The rendered "CRAP Report" (C14), fully newline-terminated.
    pub report: String,
    /// One line per source file whose coverage path did not resolve exactly.
    /// Advisory only: they never affect the exit code (C6).
    pub diagnostics: Vec<String>,
}

/// Run the pipeline and return the rendered "CRAP Report" (C14) plus any
/// resolution diagnostics.
///
/// Every failure here is an *operational* error (C6 ⇒ exit 1): unreadable or
/// unparseable LCOV, a missing path, or a source file that does not parse.
/// High-CRAP functions are **not** an error — they are simply reported, and
/// neither is a fuzzy or missing coverage match: those are diagnosed, not
/// failed.
pub fn run(cli: &Cli) -> anyhow::Result<RunOutput> {
    let lcov = coverage::load(&cli.lcov_path)
        .with_context(|| format!("failed to load LCOV file {}", cli.lcov_path.display()))?;

    let mut units = Vec::new();
    for file in discover_sources(&cli.path)? {
        let src = fs::read_to_string(&file)
            .with_context(|| format!("failed to read source file {}", file.display()))?;
        let fns = complexity::analyze_str(&src)
            .with_context(|| format!("failed to parse Rust source {}", file.display()))?;
        units.push((file.display().to_string(), fns));
    }

    let joined = join::join(&lcov, &units);
    Ok(RunOutput {
        report: report::format_report(&report::rows_from_joined(&joined.functions)),
        diagnostics: resolution_diagnostics(&joined.resolutions),
    })
}

/// Render the join's per-file resolution statuses (FC-T5b) as stderr lines.
///
/// Exact matches are silent. A non-exact (suffix) match names the LCOV key it
/// landed on, because the numbers reported for that file are only as
/// trustworthy as that guess; an unresolved file says so plainly, because its
/// functions report `N/A` (C13).
fn resolution_diagnostics(resolutions: &[(String, coverage::Resolution<'_>)]) -> Vec<String> {
    resolutions
        .iter()
        .filter_map(|(file, resolution)| match resolution {
            coverage::Resolution::Exact(_) => None,
            coverage::Resolution::Suffix(key) => Some(format!(
                "warning: {file}: no exact coverage entry; resolved by suffix match to LCOV entry {key}"
            )),
            coverage::Resolution::Unresolved => Some(format!(
                "warning: {file}: absent from the coverage profile; its functions report N/A"
            )),
        })
        .collect()
}

/// Collect the Rust sources to analyse under `root`.
///
/// A file path is taken as-is; a directory is walked recursively for `.rs`
/// files. Results are sorted so a run is deterministic regardless of directory
/// iteration order. Product-vs-test filtering is T10 and workspace member
/// enumeration is T9 — neither is pre-built here.
fn discover_sources(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    if root.is_file() {
        return Ok(vec![root.to_path_buf()]);
    }
    if !root.is_dir() {
        bail!("path not found: {}", root.display());
    }

    let mut found = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let entries = fs::read_dir(&dir)
            .with_context(|| format!("failed to read directory {}", dir.display()))?;
        for entry in entries {
            let path = entry
                .with_context(|| format!("failed to read directory {}", dir.display()))?
                .path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                found.push(path);
            }
        }
    }
    found.sort();
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_path_is_an_error() {
        let err = discover_sources(Path::new("no/such/directory")).unwrap_err();
        assert!(err.to_string().contains("path not found"));
    }

    #[test]
    fn a_file_path_is_taken_as_is() {
        let this_file = Path::new(file!());
        assert_eq!(
            discover_sources(this_file).unwrap(),
            vec![this_file.to_path_buf()]
        );
    }

    #[test]
    fn a_directory_is_walked_for_rs_files_in_sorted_order() {
        let found = discover_sources(Path::new("src")).unwrap();
        assert!(found
            .iter()
            .all(|p| p.extension().is_some_and(|e| e == "rs")));
        assert!(found.contains(&PathBuf::from("src").join("cli.rs")));
        let mut sorted = found.clone();
        sorted.sort();
        assert_eq!(found, sorted);
    }

    #[test]
    fn exact_resolutions_produce_no_diagnostics() {
        let diags = resolution_diagnostics(&[(
            "src/lib.rs".to_string(),
            coverage::Resolution::Exact("src/lib.rs"),
        )]);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn suffix_and_unresolved_resolutions_produce_one_diagnostic_each() {
        let diags = resolution_diagnostics(&[
            (
                "src/lib.rs".to_string(),
                coverage::Resolution::Exact("src/lib.rs"),
            ),
            (
                "src/fuzzy.rs".to_string(),
                coverage::Resolution::Suffix("crates/demo/src/fuzzy.rs"),
            ),
            ("src/gone.rs".to_string(), coverage::Resolution::Unresolved),
        ]);

        assert_eq!(diags.len(), 2, "{diags:?}");
        assert!(diags[0].contains("src/fuzzy.rs"), "{}", diags[0]);
        assert!(
            diags[0].contains("crates/demo/src/fuzzy.rs"),
            "{}",
            diags[0]
        );
        assert!(diags[1].contains("src/gone.rs"), "{}", diags[1]);
        assert!(diags[1].contains("N/A"), "{}", diags[1]);
    }
}
