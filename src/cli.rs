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
//! can clobber another's (FC-T8e).
//!
//! **T10 surface.** What counts as product source (C5) is decided in two
//! places, each where it can see what it needs: test *items* are skipped by the
//! CC engine ([`crate::product`] via [`crate::complexity`]), and test-only
//! `mod` declarations — and whole test-only files, subtree and all — are
//! skipped by the module-graph walk, which is where descent is decided
//! (FC-T9g). Only `--filter` selection happens here — and only on **rows**:
//! `--filter` narrows the report, never the analysis (C26), so the join sees
//! every discovered unit and a file's numbers are the same filtered and
//! unfiltered. Diagnostics are the exception that proves it: they *are* scoped
//! to the filters, because each is about one file, while attribution is settled
//! across the whole join.
//! There is no `--threshold`: v1 is a reporter, not a gate (C6/C11/D1).

use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;

use crate::diagnostic::{Kind, Site};
use crate::filter::Filters;
use crate::join::SourceUnit;
use crate::runner::CoverageSource;
use crate::{complexity, coverage, join, report, workspace};

/// `crap4rust [--lcov-path <PATH>] [--test-command <COMMAND>] [--filter
/// <FRAGMENT>]... <PATH>`.
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

    /// Only report source files whose path contains this fragment. Repeatable.
    ///
    /// A fragment matches by whole path segments against the workspace-relative
    /// path, so `--filter crates/alpha` selects `crates/alpha/src/lib.rs` but
    /// not `crates/alpha-utils/src/lib.rs`. Several are alternatives: a file is
    /// reported if **any** of them matches it. Matching is case-sensitive on
    /// every platform.
    ///
    /// This is deliberately *not* the positional `PATH` (FC-T9j): that argument
    /// locates the workspace, and a locator and a filter are different kinds of
    /// thing that clap cannot tell apart positionally.
    #[arg(long, value_name = "FRAGMENT")]
    filter: Vec<String>,

    /// A path inside the cargo workspace to analyse.
    ///
    /// Only used to find the workspace: cargo searches this path and its
    /// ancestors for the manifest, and every member of the workspace it finds
    /// is analysed (C3/A3). It narrows nothing — use `--filter` for that
    /// (FC-T9j).
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
    /// Which files reach the report (C5). Empty means all of them.
    filters: Filters,
}

impl RunConfig {
    /// Resolve a run from plain inputs — no argument parser involved (FC-T7c).
    ///
    /// Where coverage comes from — including `--lcov-path` winning outright
    /// over `--test-command` — is the coverage adapter's own policy, so it is
    /// resolved by [`CoverageSource::resolve`] rather than restated here.
    pub fn new(
        path: PathBuf,
        lcov_path: Option<PathBuf>,
        test_command: Option<&str>,
        filters: &[String],
    ) -> Self {
        let (coverage, diagnostics) = CoverageSource::resolve(lcov_path, test_command);
        Self {
            coverage,
            diagnostics,
            path,
            filters: Filters::new(filters),
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
            &cli.filter,
        )
    }
}

/// Re-exported so the binary can own the diagnostic sink [`run`] drains into
/// (FC-T9d). Only [`Display`](std::fmt::Display) is public on it.
pub use crate::diagnostic::Diagnostic;

/// The trailing **report-caveats block** — everything stdout has to say about
/// the report *as an artifact*, as distinct from the rows in it (C18/C27).
///
/// The report is the deliverable, so `crap4rust . > report.txt` must not look
/// complete when it is not: a fact about the artifact belongs on the artifact,
/// not only on the stream the artifact does not capture. There is **one** block
/// rather than a trailer per condition because T11's JSON carries these same
/// facts structurally (FC-T9n) and needs one shape to mirror, not an accreting
/// pile of ad-hoc trailing strings.
///
/// What is deliberately **not** here: anything about the *request* rather than
/// the artifact. An unmatched `--filter` stays stderr-only (C27) — stdout
/// states completeness relative to what was asked for, and a fragment that
/// matched nothing asked for nothing.
struct Caveats {
    /// Rows in the report. Zero is a caveat in its own right (C27), and gets
    /// the same line whatever caused it — a typo'd filter, an over-narrow one,
    /// a workspace that is all test code, or a genuinely empty crate — because
    /// the reader's problem, an empty artifact indistinguishable from a clean
    /// one, is the same in every case.
    rows: usize,
    /// How many **distinct modules** discovery declined to analyse (C18/C22).
    /// A lower bound: one declined `cfg_attr` path drops a subtree of unknown
    /// size.
    declined: usize,
}

impl Caveats {
    /// The block to append to the rendered report, or `""` when the report has
    /// nothing to caveat — so the happy path's bytes are exactly C14's table.
    fn render(&self) -> String {
        let mut lines = Vec::new();
        if self.rows == 0 {
            lines.push("no functions reported".to_string());
        }
        if self.declined > 0 {
            let plural = if self.declined == 1 {
                "module"
            } else {
                "modules"
            };
            let count = self.declined;
            lines.push(format!("{count} {plural} not analysed (see stderr)"));
        }
        if lines.is_empty() {
            String::new()
        } else {
            format!("\n{}\n", lines.join("\n"))
        }
    }
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
    let discovered = diagnostics.len();
    let mut units = Vec::new();
    let mut selected = BTreeSet::new();
    let mut matched = BTreeSet::new();
    // Discovery streams each file's AST as it parses it, so each file is read
    // and parsed exactly once, and only one AST is alive at a time (FC-T9h).
    workspace::discover(&config.path, diagnostics, &mut |source| {
        // C26: **every** discovered unit reaches the join, selected or not.
        // The join settles attribution over the whole claim map (C19), so a
        // unit missing from it changes the numbers of the units that remain —
        // a suffix claimant whose exact claimant was filtered away would
        // silently inherit the record and be scored from it. Selection is
        // folded here as a per-file fact and applied to rows *after* the join.
        let selection = config.filters.select(&source.path);
        matched.extend(selection.matched());
        if selection.is_selected() {
            selected.insert(source.path.clone());
        }
        units.push(SourceUnit {
            module: source.module,
            path: source.path,
            functions: complexity::analyze_file(source.ast),
        });
        Ok(())
    })?;
    scope_to_filters(&config.filters, diagnostics, discovered);
    let declined = declined(&diagnostics[discovered..]);

    let joined = join::join(&lcov, &units);
    let functions = retain_selected(joined.functions, &selected, |f| &f.file);
    let attributions = retain_selected(joined.attributions, &selected, |(file, _)| file);
    diagnostics.extend(attribution_diagnostics(&attributions));
    diagnostics.extend(
        config
            .filters
            .unmatched(&matched)
            .into_iter()
            .map(|filter| {
                Diagnostic::global(Kind::FilterMatchedNothing {
                    filter: filter.to_string(),
                })
            }),
    );

    let rows = report::rows_from_joined(&functions);
    let mut report = report::format_report(&rows);
    report.push_str(
        &Caveats {
            rows: rows.len(),
            declined,
        }
        .render(),
    );
    Ok(report)
}

/// Drop everything the filters did not select, keeping the rest in order.
///
/// This is the second half of C26 and the only place selection is *applied*:
/// the join above ran over the whole workspace, so every value here was
/// computed as if no filter had been given.
fn retain_selected<T>(
    items: Vec<T>,
    selected: &BTreeSet<String>,
    file_of: impl Fn(&T) -> &String,
) -> Vec<T> {
    items
        .into_iter()
        .filter(|item| selected.contains(file_of(item)))
        .collect()
}

/// Drop discovery diagnostics about files the filters did not select.
///
/// A diagnostic has to agree with the report it trails: complaining that a
/// module of `crates/beta` could not be resolved, under a report that was
/// explicitly narrowed to `crates/alpha`, is noise — and worse, it would be
/// counted by the C18 notice on an artifact that never claimed to cover it.
/// Diagnostics with no site are about the run as a whole and always stay.
///
/// Diagnostics are scoped; **numbers are not** (C26). Narrowing the run's
/// *voice* is safe because a diagnostic is about one file; narrowing the
/// *analysis* is not, because attribution is settled across the whole join.
///
/// **Source units are authoritative for whether a fragment matched anything.**
/// The selections made here are deliberately discarded rather than folded into
/// the run's matched set, so a fragment selecting only a file that produced a
/// diagnostic but no unit — an unparseable file, say — still reports "matched
/// no source file". That is exactly what happened: keeping its diagnostic is
/// *scoping* (the user asked about that code), not evidence that the fragment
/// selected anything to report.
fn scope_to_filters(filters: &Filters, diagnostics: &mut Vec<Diagnostic>, from: usize) {
    if filters.is_empty() {
        return;
    }
    let mut scoped = diagnostics.split_off(from);
    scoped.retain(|diagnostic| {
        diagnostic
            .site
            .as_ref()
            .is_none_or(|site| filters.select(&site.file).is_selected())
    });
    diagnostics.append(&mut scoped);
}

/// How many **distinct modules** the diagnostics report the tool declined to
/// analyse (C18/C22).
///
/// Deduped by module path, not counted per diagnostic: two cfg-guarded
/// declarations of one module are two lines worth printing but one module
/// missing from the report, and the number on the artifact is a count of
/// missing modules.
fn declined(diagnostics: &[Diagnostic]) -> usize {
    diagnostics
        .iter()
        .filter_map(|diagnostic| diagnostic.kind.declined_module())
        .collect::<BTreeSet<_>>()
        .len()
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
            &[],
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
        assert_eq!(from_parser.filters, from_plain.filters);
    }

    /// FC-T9j: the positional argument locates the workspace and nothing else.
    /// Filters are a repeatable flag, because a locator and a filter are
    /// different kinds of thing and clap cannot tell them apart positionally —
    /// `crap4rust crates/alpha crates/beta` reading as "locate at alpha, filter
    /// to beta" is indefensible, so a second positional is simply an error.
    #[test]
    fn filters_are_a_repeatable_flag_not_the_positional() {
        let cli = Cli::parse_from([
            "crap4rust",
            "--filter",
            "crates/alpha",
            "--filter",
            "crates/beta",
            ".",
        ]);
        let resolved = RunConfig::from(&cli);
        assert_eq!(resolved.path, PathBuf::from("."));
        assert_eq!(
            resolved.filters,
            Filters::new(&["crates/alpha".to_string(), "crates/beta".to_string()])
        );

        assert!(Cli::try_parse_from(["crap4rust", "crates/alpha", "crates/beta"]).is_err());
        assert!(config("crates/alpha", None, None).filters.is_empty());
    }

    /// C18: the notice is a fact about the *artifact*, so it is on the artifact
    /// — and it says how many, so an incomplete report cannot be mistaken for a
    /// complete one. One module is not "1 modules".
    #[test]
    fn the_declined_notice_agrees_with_itself_in_number() {
        let declined = |declined| Caveats { rows: 1, declined }.render();
        assert_eq!(declined(1), "\n1 module not analysed (see stderr)\n");
        assert_eq!(declined(3), "\n3 modules not analysed (see stderr)\n");
    }

    /// C27: an empty report says so on the artifact, in one shape, whatever
    /// emptied it — and a complete, non-empty report says nothing at all, so
    /// the happy path is byte-for-byte C14's table.
    #[test]
    fn the_caveats_block_is_one_block_and_is_empty_when_there_is_nothing_to_say() {
        assert_eq!(
            Caveats {
                rows: 0,
                declined: 0
            }
            .render(),
            "\nno functions reported\n"
        );
        // Both conditions at once are one block, not two trailers (FC-T9n).
        assert_eq!(
            Caveats {
                rows: 0,
                declined: 2
            }
            .render(),
            "\nno functions reported\n2 modules not analysed (see stderr)\n"
        );
        assert_eq!(
            Caveats {
                rows: 3,
                declined: 0
            }
            .render(),
            ""
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

    /// C22: the notice counts **distinct modules**, not diagnostics. Two
    /// mutually exclusive declarations of one module are two lines on stderr —
    /// both worth printing, they name different sites — but exactly one module
    /// missing from the report, and the artifact says how many *modules*.
    #[test]
    fn two_declarations_of_one_module_are_one_declined_module() {
        let missing = |line| {
            Diagnostic::run(
                Site::at("src/lib.rs", line, 1),
                Kind::ModuleFileMissing {
                    module: "demo::imp".to_string(),
                    candidates: Vec::new(),
                },
            )
        };
        let diagnostics = vec![missing(2), missing(5)];
        assert_eq!(diagnostics.len(), 2);
        assert_eq!(declined(&diagnostics), 1);
    }

    /// A narrowed report must not trail diagnostics about code it never
    /// claimed to cover — and those diagnostics must not be counted by the C18
    /// notice on it either. A site-less diagnostic is about the run as a whole
    /// and stays regardless.
    #[test]
    fn filtered_out_files_take_their_diagnostics_with_them() {
        let mut diagnostics = vec![
            Diagnostic::run(Site::file("crates/alpha/src/lib.rs"), Kind::CoverageAbsent),
            Diagnostic::run(
                Site::at("crates/beta/src/lib.rs", 1, 1),
                Kind::ModuleFileMissing {
                    module: "beta::absent".to_string(),
                    candidates: Vec::new(),
                },
            ),
            Diagnostic::global(Kind::FilterMatchedNothing {
                filter: "crates/gone".to_string(),
            }),
        ];

        scope_to_filters(
            &Filters::new(&["crates/alpha".to_string()]),
            &mut diagnostics,
            0,
        );

        assert_eq!(diagnostics.len(), 2, "{:?}", rendered(&diagnostics));
        assert!(diagnostics[0].to_string().contains("crates/alpha"));
        assert!(diagnostics[1].to_string().contains("crates/gone"));
        assert_eq!(declined(&diagnostics), 0);
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
