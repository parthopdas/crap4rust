//! CLI edge (T7) — argument parsing and pipeline composition.
//!
//! This is the composition root: it asks the coverage adapter for an LCOV file
//! and the discovery adapter for the workspace's sources, reads and analyses
//! each source, and drives the pure core (CC engine → join → reporter).
//! Everything it calls inward is pure.
//!
//! **Process concerns stay in `main.rs`.** [`run`] is fallible and *returns*
//! the rendered report, pushing everything it has to say into the caller's
//! diagnostic sink as it goes (FC-T9d), so the whole pipeline is exercisable
//! without capturing process output; `main` drains the sink to stderr — on
//! `Err` as well as `Ok` — maps `Err` to exit code 1, and prints the report to
//! stdout (C6).
//!
//! **S2/S3 surface.** Coverage is zero-config (C2): without `--lcov-path` the
//! coverage command runs first and the LCOV artifact *it* produced is read
//! back. Which of the two it is, and the whole artifact lifecycle around a
//! generated one, is the runner adapter's policy ([`crate::runner`]). Source
//! discovery is likewise the discovery adapter's policy
//! ([`crate::workspace`]) — workspace members, crate-qualified module names and
//! the path each file is known by all come from there (FC-T7d). Coverage is
//! generated **once per invocation** and discovery runs per member: `cargo
//! llvm-cov` is workspace-aware, so there is exactly one artifact and no root
//! can clobber another's (FC-T8e). Product-vs-test filtering is still T10.
//! There is no `--threshold`: v1 is a reporter, not a gate (C6/C11/D1).

use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;

use crate::diagnostic::{Kind, Site};
use crate::join::SourceUnit;
use crate::runner::CoverageSource;
use crate::{complexity, coverage, join, report, workspace};

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

    /// A path inside the cargo workspace to analyse.
    ///
    /// Only used to find the workspace: cargo searches this path and its
    /// ancestors for the manifest, and every member of the workspace it finds
    /// is analysed (C3/A3).
    #[arg(value_name = "PATH")]
    path: PathBuf,
}

/// A run's resolved inputs (FC-T7c): defaults and flag precedence are settled
/// once, here, so [`run`] is exercisable without an argument parser.
pub struct RunConfig {
    /// Where the LCOV profile comes from, and whether it is ours to manage.
    coverage: CoverageSource,
    /// Config-phase diagnostics produced while resolving the coverage source
    /// (e.g. an ignored `--test-command`). Advisory only: they never affect the
    /// exit code (C6).
    diagnostics: Vec<Diagnostic>,
    /// A path inside the cargo workspace to analyse.
    path: PathBuf,
}

impl RunConfig {
    /// Resolve a run from plain inputs — no argument parser involved (FC-T7c).
    ///
    /// Where coverage comes from — including `--lcov-path` winning outright
    /// over `--test-command` — is the coverage adapter's own policy, so it is
    /// resolved by [`CoverageSource::resolve`] rather than restated here.
    pub fn new(path: PathBuf, lcov_path: Option<PathBuf>, test_command: Option<&str>) -> Self {
        let (coverage, diagnostics) = CoverageSource::resolve(lcov_path, test_command);
        Self {
            coverage,
            diagnostics,
            path,
        }
    }

    /// What is already known about the resolved inputs, to be drained *before*
    /// the run starts (FC-T8g): a coverage command that will never run, or one
    /// that must write LCOV itself, is worth saying before a long test run
    /// rather than after it. Advisory only — never an exit-code effect (C6).
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
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

/// Re-exported so the binary can own the diagnostic sink [`run`] drains into
/// (FC-T9d). Only [`Display`](std::fmt::Display) is public on it.
pub use crate::diagnostic::Diagnostic;

/// Trailing stdout notice when discovery declined work (C18).
///
/// The report *is* the deliverable — `crap4rust . > report.txt` must not look
/// complete when it is not — so the fact that modules are missing from it
/// belongs in the artifact, not only on the stream the artifact does not
/// capture.
fn declined_notice(count: usize) -> String {
    let plural = if count == 1 { "module" } else { "modules" };
    format!("\n{count} {plural} not analysed (see stderr)\n")
}

/// Run the pipeline and return the rendered "CRAP Report" (C14).
///
/// Everything the run has to *say* is pushed into `diagnostics` as it is
/// produced, so it survives a later failure (FC-T9d): a workspace with three
/// unresolvable modules and one unparseable file must still say all four
/// things, not just the error.
///
/// Every failure here is an *operational* error (C6 ⇒ exit 1): a coverage
/// command that cannot be run or exits non-zero, unreadable or unparseable
/// LCOV, a path that is not in a cargo workspace, or a source file that does
/// not parse. High-CRAP functions are **not** an error — they are simply
/// reported, and neither is a fuzzy, ambiguous or missing coverage match: those
/// are diagnosed, not failed.
pub fn run(config: &RunConfig, diagnostics: &mut Vec<Diagnostic>) -> anyhow::Result<String> {
    let lcov_path = config.coverage.ensure_lcov()?;

    let lcov = coverage::load(lcov_path)
        .with_context(|| format!("failed to load LCOV file {}", lcov_path.display()))?;

    // Discovery diagnostics come first: a declared module the graph could not
    // reach explains why a file is missing from everything that follows.
    let declined_before = declined(diagnostics);
    let mut units = Vec::new();
    // Discovery streams each file's AST as it parses it, so each file is read
    // and parsed exactly once, and only one AST is alive at a time (FC-T9h).
    workspace::discover(&config.path, diagnostics, &mut |source| {
        units.push(SourceUnit {
            module: source.module,
            path: source.path,
            functions: complexity::analyze_file(source.ast),
        });
        Ok(())
    })?;
    let declined = declined(diagnostics) - declined_before;

    let joined = join::join(&lcov, &units);
    diagnostics.extend(attribution_diagnostics(&joined.attributions));

    let mut report = report::format_report(&report::rows_from_joined(&joined.functions));
    if declined > 0 {
        report.push_str(&declined_notice(declined));
    }
    Ok(report)
}

/// How many diagnostics so far report work the tool declined (C18).
fn declined(diagnostics: &[Diagnostic]) -> usize {
    diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.kind.declines_analysis())
        .count()
}

/// Render the join's per-file attributions (FC-T5b) as diagnostics.
///
/// Exact matches are silent. A non-exact (suffix) match names the LCOV key it
/// landed on, because the numbers reported for that file are only as
/// trustworthy as that guess; an ambiguous match names *every* key it was torn
/// between, because that set is what the user has to disambiguate; a collision
/// names the contested key *and* every source file claiming it, because the
/// mapping — not the file — is what has to be fixed; a superseded claim names
/// the file that proved the better claim (C19); an unresolved file says so
/// plainly. All but the first two report `N/A` for their functions (C13).
fn attribution_diagnostics(attributions: &[(String, join::Attribution<'_>)]) -> Vec<Diagnostic> {
    attributions
        .iter()
        .filter_map(|(file, attribution)| {
            let kind = match attribution {
                join::Attribution::Resolved(coverage::Resolution::Exact(_)) => return None,
                join::Attribution::Resolved(coverage::Resolution::Suffix(key)) => {
                    Kind::CoverageSuffixMatch {
                        key: (*key).to_string(),
                    }
                }
                join::Attribution::Resolved(coverage::Resolution::Ambiguous(keys)) => {
                    Kind::CoverageAmbiguous {
                        keys: keys.iter().map(|key| (*key).to_string()).collect(),
                    }
                }
                join::Attribution::Resolved(coverage::Resolution::Unresolved) => {
                    Kind::CoverageAbsent
                }
                join::Attribution::Collision { key, sources } => Kind::CoverageContested {
                    key: (*key).to_string(),
                    claimants: sources.clone(),
                },
                join::Attribution::Superseded { key, winner } => Kind::CoverageSuperseded {
                    key: (*key).to_string(),
                    winner: winner.clone(),
                },
            };
            Some(Diagnostic::run(Site::file(file), kind))
        })
        .collect()
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

    /// Rendered form of each diagnostic — what stderr would show.
    fn rendered(diagnostics: &[Diagnostic]) -> Vec<String> {
        diagnostics.iter().map(Diagnostic::to_string).collect()
    }

    #[test]
    fn parsed_arguments_resolve_the_same_way_as_plain_inputs() {
        // `From<&Cli>` must be a thin adapter over `RunConfig::new`, not a
        // second resolution path.
        let cli = Cli::parse_from(["crap4rust", "--test-command", "my-tool --out {lcov}", "src"]);
        let from_parser = RunConfig::from(&cli);
        let from_plain = config("src", None, Some("my-tool --out {lcov}"));
        assert_eq!(from_parser.coverage, from_plain.coverage);
        assert_eq!(from_parser.diagnostics, from_plain.diagnostics);
        assert_eq!(from_parser.path, from_plain.path);
    }

    /// C18: the notice is a fact about the *artifact*, so it is on the artifact
    /// — and it says how many, so an incomplete report cannot be mistaken for a
    /// complete one. One module is not "1 modules".
    #[test]
    fn the_declined_notice_agrees_with_itself_in_number() {
        assert_eq!(declined_notice(1), "\n1 module not analysed (see stderr)\n");
        assert_eq!(
            declined_notice(3),
            "\n3 modules not analysed (see stderr)\n"
        );
    }

    /// C18 counts declined *work*: a module that is not in the report at all.
    /// A coverage miss is not one — those functions are reported, with N/A.
    #[test]
    fn only_declined_work_is_counted_for_the_notice() {
        let diagnostics = vec![
            Diagnostic::run(
                Site::at("src/lib.rs", 1, 1),
                Kind::ModuleFileMissing {
                    module: "demo::absent".to_string(),
                    candidates: Vec::new(),
                },
            ),
            Diagnostic::run(Site::file("src/lib.rs"), Kind::CoverageAbsent),
        ];
        assert_eq!(declined(&diagnostics), 1);
    }

    #[test]
    fn exact_resolutions_produce_no_diagnostics() {
        let diags = attribution_diagnostics(&[(
            "src/lib.rs".to_string(),
            join::Attribution::Resolved(coverage::Resolution::Exact("src/lib.rs")),
        )]);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn suffix_and_unresolved_resolutions_produce_one_diagnostic_each() {
        let diags = rendered(&attribution_diagnostics(&[
            (
                "src/lib.rs".to_string(),
                join::Attribution::Resolved(coverage::Resolution::Exact("src/lib.rs")),
            ),
            (
                "src/fuzzy.rs".to_string(),
                join::Attribution::Resolved(coverage::Resolution::Suffix(
                    "crates/demo/src/fuzzy.rs",
                )),
            ),
            (
                "src/gone.rs".to_string(),
                join::Attribution::Resolved(coverage::Resolution::Unresolved),
            ),
        ]));

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

    /// C19: a claim that lost to an exact match is still diagnosed, and says
    /// *where* the record went — "absent from the coverage profile" would be a
    /// lie, the record exists and belongs to a named file.
    #[test]
    fn a_superseded_claim_names_the_record_and_its_winner() {
        let diags = rendered(&attribution_diagnostics(&[(
            "crates/beta/src/lib.rs".to_string(),
            join::Attribution::Superseded {
                key: "src/lib.rs",
                winner: "src/lib.rs".to_string(),
            },
        )]));

        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(
            diags[0].contains(
                "crates/beta/src/lib.rs: LCOV entry src/lib.rs matches src/lib.rs exactly"
            ),
            "{}",
            diags[0]
        );
        assert!(diags[0].contains("N/A"), "{}", diags[0]);
    }

    /// FC-T5a: a tie must be *actionable* — "ambiguous, good luck" is worse
    /// than no warning, so the diagnostic names every tied candidate.
    #[test]
    fn an_ambiguous_resolution_names_the_tied_candidates() {
        let diags = rendered(&attribution_diagnostics(&[(
            "src/lib.rs".to_string(),
            join::Attribution::Resolved(coverage::Resolution::Ambiguous(vec![
                "crates/alpha/src/lib.rs",
                "crates/beta/src/lib.rs",
            ])),
        )]));

        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].contains("ambiguous"), "{}", diags[0]);
        assert!(diags[0].contains("crates/alpha/src/lib.rs"), "{}", diags[0]);
        assert!(diags[0].contains("crates/beta/src/lib.rs"), "{}", diags[0]);
        assert!(diags[0].contains("N/A"), "{}", diags[0]);
    }

    /// Defect 3: a contested LCOV record must name the record *and* every
    /// source file claiming it — the many-to-one mapping is what has to be
    /// fixed, and neither claimant can be identified from its own line alone.
    #[test]
    fn a_collision_names_the_contested_key_and_every_claimant() {
        let sources = vec![
            "src/lib.rs".to_string(),
            "crates/beta/src/lib.rs".to_string(),
        ];
        let diags = rendered(&attribution_diagnostics(&[
            (
                "src/lib.rs".to_string(),
                join::Attribution::Collision {
                    key: "src/lib.rs",
                    sources: sources.clone(),
                },
            ),
            (
                "crates/beta/src/lib.rs".to_string(),
                join::Attribution::Collision {
                    key: "src/lib.rs",
                    sources,
                },
            ),
        ]));

        assert_eq!(diags.len(), 2, "{diags:?}");
        for diag in &diags {
            // The claimant list is asserted whole: checking for `src/lib.rs`
            // alone would be satisfied by the contested LCOV key of the same
            // name, so neither claimant would actually be required.
            assert!(
                diag.contains(
                    "LCOV entry src/lib.rs is claimed by src/lib.rs, crates/beta/src/lib.rs"
                ),
                "{diag}"
            );
            assert!(diag.contains("N/A"), "{diag}");
        }
    }
}
