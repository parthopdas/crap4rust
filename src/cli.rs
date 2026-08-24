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
//! **S2 surface.** Coverage is zero-config (C2): without `--lcov-path` the
//! coverage command runs first and the LCOV artifact *it* produced is read
//! back. Which of the two it is, and the whole artifact lifecycle around a
//! generated one, is the runner adapter's policy ([`crate::runner`]) — this
//! module only asks it for a path to read. Source discovery is still a
//! deliberately minimal `.rs` walk because workspace enumeration is T9 and
//! product/test filtering is T10. There is no `--threshold`: v1 is a reporter,
//! not a gate (C6/C11/D1).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use clap::Parser;

use crate::runner::CoverageSource;
use crate::{complexity, coverage, join, report};

/// `crap4rust [--lcov-path <PATH>] [--test-command <COMMAND>] <PATH>`.
#[derive(Parser)]
#[command(name = "crap4rust", version, about, long_about = None)]
pub struct Cli {
    /// Use an existing LCOV coverage file instead of running coverage.
    ///
    /// This is the bring-your-own-coverage seam: when it is given, no coverage
    /// command is run at all (so `--test-command` is ignored). Without it,
    /// coverage runs and is read back from `target/crap4rust/coverage.lcov`.
    #[arg(long, value_name = "PATH")]
    lcov_path: Option<PathBuf>,

    /// Coverage command to run instead of `cargo llvm-cov`.
    ///
    /// Split on whitespace and executed directly — there is no shell, so
    /// quoting, pipes and redirection are not supported, and a program or
    /// argument containing spaces cannot be expressed. The literal token
    /// `{lcov}` in any argument is replaced with the LCOV output path; unlike
    /// crap4go's `{coverprofile}`, which expands to a whole flag, `{lcov}`
    /// expands to a bare path and so needs its own flag in front of it (e.g.
    /// `--output-path {lcov}`). A command with no `{lcov}` token must write
    /// LCOV to `target/crap4rust/coverage.lcov` itself. The default command is
    /// `cargo llvm-cov --lcov --output-path {lcov}`.
    #[arg(long, value_name = "COMMAND")]
    test_command: Option<String>,

    /// Rust source file, or directory to scan recursively for `.rs` files.
    #[arg(value_name = "PATH")]
    path: PathBuf,
}

/// A run's resolved inputs (FC-T7c): defaults and flag precedence are settled
/// once, here, so [`run`] is exercisable without an argument parser.
pub struct RunConfig {
    /// Where the LCOV profile comes from, and whether it is ours to manage.
    coverage: CoverageSource,
    /// Advisory lines produced while resolving the coverage source (e.g. an
    /// ignored `--test-command`). Advisory only: they never affect the exit
    /// code (C6).
    advisories: Vec<String>,
    /// Rust source file or directory to analyse.
    path: PathBuf,
}

impl RunConfig {
    /// Resolve a run from plain inputs — no argument parser involved (FC-T7c).
    ///
    /// Where coverage comes from — including `--lcov-path` winning outright
    /// over `--test-command` — is the coverage adapter's own policy, so it is
    /// resolved by [`CoverageSource::resolve`] rather than restated here.
    pub fn new(path: PathBuf, lcov_path: Option<PathBuf>, test_command: Option<&str>) -> Self {
        let (coverage, advisories) = CoverageSource::resolve(lcov_path, test_command);
        Self {
            coverage,
            advisories,
            path,
        }
    }

    /// Advisories about the resolved coverage inputs, to be emitted *before*
    /// the run starts: a coverage command that will never run, or one that must
    /// write LCOV itself, is worth saying before a long test run rather than
    /// after it. Advisory only — they never affect the exit code (C6).
    pub fn advisories(&self) -> &[String] {
        &self.advisories
    }
}

impl From<&Cli> for RunConfig {
    /// Parsed arguments are just one source of the plain inputs
    /// [`RunConfig::new`] resolves — there is no second resolution path.
    fn from(cli: &Cli) -> Self {
        Self::new(
            cli.path.clone(),
            cli.lcov_path.clone(),
            cli.test_command.as_deref(),
        )
    }
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
/// Every failure here is an *operational* error (C6 ⇒ exit 1): a coverage
/// command that cannot be run or exits non-zero, unreadable or unparseable
/// LCOV, a missing path, or a source file that does not parse. High-CRAP
/// functions are **not** an error — they are simply reported, and neither is a
/// fuzzy or missing coverage match: those are diagnosed, not failed.
pub fn run(config: &RunConfig) -> anyhow::Result<RunOutput> {
    let lcov_path = config.coverage.ensure_lcov()?;

    let lcov = coverage::load(lcov_path)
        .with_context(|| format!("failed to load LCOV file {}", lcov_path.display()))?;

    let mut units = Vec::new();
    for file in discover_sources(&config.path)? {
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

    /// Build a config the way a caller with plain inputs does — no parser.
    fn config(path: &str, lcov_path: Option<&str>, test_command: Option<&str>) -> RunConfig {
        RunConfig::new(
            PathBuf::from(path),
            lcov_path.map(PathBuf::from),
            test_command,
        )
    }

    #[test]
    fn parsed_arguments_resolve_the_same_way_as_plain_inputs() {
        // `From<&Cli>` must be a thin adapter over `RunConfig::new`, not a
        // second resolution path.
        let cli = Cli::parse_from(["crap4rust", "--test-command", "my-tool --out {lcov}", "src"]);
        let from_parser = RunConfig::from(&cli);
        let from_plain = config("src", None, Some("my-tool --out {lcov}"));
        assert_eq!(from_parser.coverage, from_plain.coverage);
        assert_eq!(from_parser.advisories, from_plain.advisories);
        assert_eq!(from_parser.path, from_plain.path);
    }

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
