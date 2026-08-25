//! Coverage join (T5) — pure, I/O-free.
//!
//! Turns per-file cyclomatic-complexity results ([`FunctionComplexity`], which
//! carry **no** file path) plus already-parsed [`LcovData`] into per-function
//! coverage fractions and CRAP scores. It consumes only in-memory values, so
//! there is no filesystem or process I/O here.
//!
//! Two concerns live here:
//!
//! 1. **Path resolution** — LCOV `SF:` paths and the CC engine's source paths
//!    do not match byte-for-byte (relative vs absolute, `./` prefix, `\` vs
//!    `/`). A naive exact lookup silently returns `(0,0)` ⇒ `cov=0` ⇒ inflated
//!    CRAP on every function. We delegate to [`LcovData::resolve_path`], which
//!    follows crap4go's normalize + segment-suffix matching (normalization is
//!    verified-identical; the *bidirectional* suffix match is a deliberate,
//!    documented divergence — see [`LcovData::resolve_path`]), and keep the
//!    file-absent vs file-present distinction all the way to the join contract.
//!    The *exactness* of each match is carried out as data
//!    ([`JoinResult::attributions`]) so the CLI can emit a stderr diagnostic
//!    (FC-T5b) — this module never prints.
//!    Resolution is decided over the **whole join**, not per file in isolation:
//!    see [`Attribution`] for why one LCOV record may never be attributed to
//!    two different source files.
//! 2. **The C13 join contract** — file-absent ⇒ `None` (N/A, unscored);
//!    file-present with no instrumented lines (`total==0`) ⇒ `Some(1.0)` (the
//!    deliberate divergence from crap4go); otherwise `Some(covered/total)`.
//!
//! Sorting, `N/A`-last ordering and the table columns (C14) are the reporter's
//! job (T6/T7), not this module — we only carry the crate-qualified `module`
//! (C3, derived upstream by the discovery adapter), `file` and `start_line`
//! through.

use std::collections::BTreeMap;

use crate::complexity::FunctionComplexity;
use crate::coverage::{LcovData, Resolution};
use crate::crap;

/// One function after the coverage join: its complexity, resolved coverage
/// fraction (per the C13 contract), and CRAP score.
pub(crate) struct JoinedFunction {
    /// Qualified display name, carried straight from [`FunctionComplexity`].
    pub(crate) name: String,
    /// Full module path of the function (C3/C20): the source unit's
    /// crate-qualified module path plus the inline `mod` segments enclosing the
    /// function. Carried through for the reporter's Module column.
    pub(crate) module: String,
    /// Source file path (as given to the join). Carried through for the C15
    /// identity key; not normalized here.
    pub(crate) file: String,
    /// 1-based start line of the function, carried straight from
    /// [`FunctionComplexity`]. With `file` it forms the stable identity key
    /// (C15) that disambiguates functions sharing a display name.
    pub(crate) start_line: usize,
    /// Cyclomatic complexity.
    pub(crate) complexity: u32,
    /// Coverage fraction in `[0.0, 1.0]`, or `None` when the file is absent
    /// from the LCOV profile (N/A, unscored). Never NaN or out of range.
    pub(crate) coverage: Option<f64>,
    /// CRAP score = `crap::score(complexity, coverage)`; `None` mirrors an
    /// absent coverage.
    pub(crate) crap: Option<f64>,
}

/// One analysed source file: the functions the CC engine found in it, plus the
/// identity the CC engine does not carry — which package's module this file is,
/// and the path it is known by.
///
/// This replaces the anonymous `(String, Vec<FunctionComplexity>)` tuple
/// (FC-T5e). `module` is **crate-qualified** (C3) and is derived upstream, in
/// the discovery adapter where package identity actually exists — the join and
/// the reporter only carry it. The crate name is the module's first segment, so
/// package identity is present without a second, write-only field.
pub(crate) struct SourceUnit {
    /// Crate-qualified module path (C3), e.g. `demo::foo::bar`.
    pub(crate) module: String,
    /// The path this file is known by — workspace-relative and forward-slashed
    /// as produced by discovery. Both the LCOV query key and the C15 identity
    /// `file`.
    pub(crate) path: String,
    /// The functions the CC engine found in this file.
    pub(crate) functions: Vec<FunctionComplexity>,
}

/// What one source file's coverage was finally attributed to, over the
/// **complete** join.
///
/// [`LcovData::resolve_path`] answers one query at a time, so it can only ever
/// see one side of a many-to-one mapping: with a single `SF:src/lib.rs` record,
/// every member's `src/lib.rs` resolves to it, each query finding exactly one
/// candidate and therefore never a tie. Every member would then be scored from
/// the *same* record — confidently, and for all but one of them wrongly.
///
/// So attribution is settled here, where the whole join is visible: an LCOV
/// record claimed by more than one distinct source path belongs to none of them
/// ([`Collision`](Self::Collision), reported and unscored), because nothing in
/// the data proves which file it describes. An LCOV path is only ever relative
/// to *its* build root, which we do not know — so even a byte-for-byte match on
/// a workspace-relative path is not proof of ownership once a second file
/// claims the same record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Attribution<'a> {
    /// The file's own resolution stands: it is the only claimant of its key, or
    /// the only one that matched it *exactly* (C19), or it has no key at all
    /// (absent, or an ambiguous tie).
    Resolved(Resolution<'a>),
    /// Several distinct source paths claim one LCOV record and none of them
    /// outranks the others, so it is attributed to none of them: their
    /// functions report `N/A` (C13).
    Collision {
        /// The contested LCOV key.
        key: &'a str,
        /// Every source path claiming it, in join input order.
        sources: Vec<String>,
    },
    /// This file claimed a record by suffix match and lost it to the single
    /// claimant that matched it exactly (C19). Its functions report `N/A` — the
    /// record is not its, and no other record is either.
    Superseded {
        /// The contested LCOV key.
        key: &'a str,
        /// The source path that matched `key` exactly and therefore owns it.
        winner: String,
    },
}

impl<'a> Attribution<'a> {
    /// The LCOV key this file's coverage may be read from; `None` whenever
    /// there is no single defensible record — absent, ambiguous, contested, or
    /// lost to an exact claimant.
    fn key(&self) -> Option<&'a str> {
        match self {
            Self::Resolved(resolution) => resolution.key(),
            Self::Collision { .. } | Self::Superseded { .. } => None,
        }
    }
}

/// The join's output: the per-function results plus the per-file attributions
/// that produced them.
pub(crate) struct JoinResult<'a> {
    /// One entry per input function, in input order.
    pub(crate) functions: Vec<JoinedFunction>,
    /// One entry per input file, in input order: the path as given, and what
    /// its coverage was attributed to (FC-T5b). Borrowed keys point into the
    /// [`LcovData`] the join was run against.
    pub(crate) attributions: Vec<(String, Attribution<'a>)>,
}

/// Join per-file CC results against `lcov`, producing one [`JoinedFunction`]
/// per input function plus how each input file's coverage was attributed
/// (FC-T5b).
///
/// Output order follows the input; sorting/formatting is the reporter's job
/// (C14).
///
/// Three passes, because attribution needs the whole join in view (see
/// [`Attribution`]): resolve every file's path, collect the claims on each LCOV
/// key, then score only the files whose claim is uncontested. Both the claim
/// map (`BTreeMap`) and the per-key claimant lists (input order) are
/// deterministic (FC-T6b).
///
/// The join stays **pure**: it *reports* attribution as data and never prints.
/// Turning a non-exact, ambiguous, contested or unresolved match into a stderr
/// diagnostic is the CLI edge's job.
pub(crate) fn join<'a>(lcov: &'a LcovData, units: &[SourceUnit]) -> JoinResult<'a> {
    // 1. Resolve each file once — not once per function.
    let resolved: Vec<Resolution<'a>> = units
        .iter()
        .map(|unit| lcov.resolve_path(&unit.path))
        .collect();

    // 2. Who claims which LCOV record, and how strongly. Distinct *paths*
    //    only: the same file listed twice is one claimant, not a collision with
    //    itself. `Resolution` already records *how* a file matched, and C19 is
    //    exactly the moment that stops being decoration: an exact claim is
    //    evidence a suffix claim does not have.
    let mut claims: BTreeMap<&'a str, Vec<Claim>> = BTreeMap::new();
    for (unit, resolution) in units.iter().zip(&resolved) {
        if let Some(key) = resolution.key() {
            let claimants = claims.entry(key).or_default();
            if !claimants.iter().any(|claim| claim.path == unit.path) {
                claimants.push(Claim {
                    path: unit.path.clone(),
                    exact: matches!(resolution, Resolution::Exact(_)),
                });
            }
        }
    }

    // 3. Score, skipping every file whose record another file has a better — or
    //    an equally good — claim on.
    let mut functions = Vec::new();
    let mut attributions = Vec::new();
    for (unit, resolution) in units.iter().zip(resolved) {
        let attribution = match resolution.key().map(|key| (key, &claims[key])) {
            Some((key, claimants)) if claimants.len() > 1 => {
                contested(key, claimants, &unit.path, resolution)
            }
            _ => Attribution::Resolved(resolution),
        };
        let key = attribution.key();
        for fc in &unit.functions {
            let coverage = coverage_for(lcov, key, fc);
            let crap = crap::score(fc.complexity, coverage);
            functions.push(JoinedFunction {
                name: fc.name.clone(),
                module: module_of(unit, fc),
                file: unit.path.clone(),
                start_line: fc.start_line,
                complexity: fc.complexity,
                coverage,
                crap,
            });
        }
        attributions.push((unit.path.clone(), attribution));
    }
    JoinResult {
        functions,
        attributions,
    }
}

/// One source file's claim on an LCOV record, with the evidence behind it.
struct Claim {
    path: String,
    /// The claim came from an [`Resolution::Exact`] match — the record's own
    /// key *is* this file's path, not merely a suffix of it.
    exact: bool,
}

/// Settle a contested LCOV record between its claimants (C19).
///
/// With **exactly one** exact claimant, the evidence is not symmetric: that
/// file's path *is* the record's key, while every other claimant only shares a
/// path suffix with it. The exact claimant takes the record, and the losers are
/// [`Superseded`](Attribution::Superseded) — reported, unscored (C13), and each
/// told where its record went.
///
/// Any other shape — no exact claimant, or two of them — is a genuine
/// [`Collision`](Attribution::Collision): nothing in the data ranks the claims,
/// so the record is attributed to none of them and every claimant is named.
fn contested<'a>(
    key: &'a str,
    claimants: &[Claim],
    path: &str,
    resolution: Resolution<'a>,
) -> Attribution<'a> {
    let mut exact = claimants.iter().filter(|claim| claim.exact);
    match (exact.next(), exact.next()) {
        (Some(winner), None) if winner.path == path => Attribution::Resolved(resolution),
        (Some(winner), None) => Attribution::Superseded {
            key,
            winner: winner.path.clone(),
        },
        _ => Attribution::Collision {
            key,
            sources: claimants.iter().map(|claim| claim.path.clone()).collect(),
        },
    }
}

/// The function's full module path (C20): the file's crate-qualified module
/// path, plus the inline `mod` segments enclosing the function.
///
/// Both halves are needed and neither knows the other: crate and file identity
/// exist only in discovery, inline `mod` nesting only in the AST. Composing
/// them here is what makes `module` mean "the module this function is in" for
/// *every* function, rather than "as much of it as one side happened to see".
fn module_of(unit: &SourceUnit, fc: &FunctionComplexity) -> String {
    if fc.module_path.is_empty() {
        unit.module.clone()
    } else {
        format!("{}::{}", unit.module, fc.module_path.join("::"))
    }
}

/// Apply the C13 contract for one function given its file's attributed LCOV key.
///
/// The returned `Some` fraction is always in `[0.0, 1.0]` and never NaN:
/// `covered ≤ total` by construction in [`LcovData::coverage_in_range`], and
/// the `total == 0` branch short-circuits before any division (FC-T4b).
fn coverage_for(lcov: &LcovData, key: Option<&str>, fc: &FunctionComplexity) -> Option<f64> {
    // No defensible key (file absent, an ambiguous tie, or a record contested
    // by another source file) ⇒ None: N/A, unscored.
    let key = key?;
    let (covered, total) =
        lcov.coverage_in_range(key, line_to_u32(fc.start_line), line_to_u32(fc.end_line));
    if total == 0 {
        // C13 divergence from crap4go: no instrumented lines ⇒ nothing to test.
        Some(1.0)
    } else {
        Some(covered as f64 / total as f64)
    }
}

/// Consciously narrow a 1-based `usize` source line to the `u32` that
/// [`LcovData::coverage_in_range`] expects (FC-T3). Real files never approach
/// `u32::MAX` lines; saturating avoids a panic on the theoretically-unreachable
/// overflow instead of `unwrap`/`expect` on a reachable path.
fn line_to_u32(line: usize) -> u32 {
    u32::try_from(line).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coverage::parse_lcov;

    /// LCOV with a single file `src/lib.rs`:
    /// - lines 10,11 hit; 12,13 uncovered  → range 10..=13 = 2/4
    /// - lines 20,21 hit                    → range 20..=21 = 2/2
    /// - no DA lines >= 100                 → range 100..=110 = 0/0
    const FIXTURE: &str = "\
SF:src/lib.rs
DA:10,1
DA:11,4
DA:12,0
DA:13,0
DA:20,5
DA:21,3
end_of_record
";

    fn fc(name: &str, complexity: u32, start_line: usize, end_line: usize) -> FunctionComplexity {
        FunctionComplexity {
            name: name.to_string(),
            module_path: Vec::new(),
            complexity,
            start_line,
            end_line,
        }
    }

    fn unit(path: &str, functions: Vec<FunctionComplexity>) -> SourceUnit {
        SourceUnit {
            module: "demo".to_string(),
            path: path.to_string(),
            functions,
        }
    }

    fn join_one(file: &str, fc: FunctionComplexity) -> JoinedFunction {
        let lcov = parse_lcov(FIXTURE).expect("valid fixture");
        let mut out = join(&lcov, &[unit(file, vec![fc])]).functions;
        assert_eq!(out.len(), 1);
        out.pop().unwrap()
    }

    #[test]
    fn partial_coverage_is_fraction_and_crap_via_score() {
        // 2 of 4 instrumented lines hit ⇒ 0.5.
        let j = join_one("src/lib.rs", fc("partial", 2, 10, 13));
        assert_eq!(j.coverage, Some(0.5));
        assert_eq!(j.crap, crap::score(2, Some(0.5)));
    }

    #[test]
    fn zero_coverage_is_zero() {
        // Lines 12,13 both uncovered ⇒ 0 of 2 ⇒ 0.0.
        let j = join_one("src/lib.rs", fc("zero", 3, 12, 13));
        assert_eq!(j.coverage, Some(0.0));
        assert_eq!(j.crap, crap::score(3, Some(0.0)));
    }

    #[test]
    fn full_coverage_is_one() {
        // Lines 20,21 both hit ⇒ 2 of 2 ⇒ 1.0.
        let j = join_one("src/lib.rs", fc("full", 4, 20, 21));
        assert_eq!(j.coverage, Some(1.0));
        assert_eq!(j.crap, crap::score(4, Some(1.0)));
    }

    #[test]
    fn file_absent_is_none_and_unscored() {
        // File not present in the LCOV profile ⇒ N/A, unscored.
        let j = join_one("src/other.rs", fc("absent", 7, 10, 13));
        assert_eq!(j.coverage, None);
        assert_eq!(j.crap, None);
    }

    #[test]
    fn the_crate_qualified_module_is_carried_through() {
        // C3/FC-T6a: the Module string is derived upstream and only carried
        // here — the join never looks at the file path to invent one.
        let lcov = parse_lcov(FIXTURE).expect("valid fixture");
        let units = vec![SourceUnit {
            module: "alpha::foo".to_string(),
            path: "crates/alpha/src/foo.rs".to_string(),
            functions: vec![fc("f", 1, 10, 13)],
        }];
        let joined = join(&lcov, &units).functions;
        assert_eq!(joined[0].module, "alpha::foo");
        assert_eq!(joined[0].file, "crates/alpha/src/foo.rs");
    }

    #[test]
    fn an_ambiguous_path_is_unscored_and_reported() {
        // FC-T5a: two members' keys match the query equally well, so there is
        // no defensible coverage to report — N/A (C13), plus the tie as data.
        let lcov = parse_lcov(
            "SF:crates/alpha/src/lib.rs\nDA:10,1\nend_of_record\n\
             SF:crates/beta/src/lib.rs\nDA:10,0\nend_of_record\n",
        )
        .expect("valid fixture");
        let result = join(&lcov, &[unit("src/lib.rs", vec![fc("f", 3, 10, 13)])]);

        assert_eq!(result.functions[0].coverage, None);
        assert_eq!(result.functions[0].crap, None);
        assert_eq!(
            result.attributions,
            vec![(
                "src/lib.rs".to_string(),
                Attribution::Resolved(Resolution::Ambiguous(vec![
                    "crates/alpha/src/lib.rs",
                    "crates/beta/src/lib.rs"
                ]))
            )]
        );
    }

    /// Defect 3 (the many-to-one case `resolve_path` cannot see): a single LCOV
    /// record and two members whose paths both resolve to it *by suffix*. Each
    /// query has exactly one candidate, so neither is *ambiguous* — yet the
    /// record can describe only one of them, and nothing says which. Both must
    /// be N/A.
    #[test]
    fn one_lcov_record_claimed_by_two_sources_is_a_collision_and_unscored() {
        let lcov = parse_lcov("SF:src/lib.rs\nDA:10,1\nend_of_record\n").expect("valid fixture");
        let units = vec![
            SourceUnit {
                module: "alpha".to_string(),
                path: "crates/alpha/src/lib.rs".to_string(),
                functions: vec![fc("alpha_one", 2, 10, 13)],
            },
            SourceUnit {
                module: "beta".to_string(),
                path: "crates/beta/src/lib.rs".to_string(),
                functions: vec![fc("beta_one", 1, 10, 13)],
            },
        ];
        let result = join(&lcov, &units);

        for joined in &result.functions {
            assert_eq!(joined.coverage, None, "{} was scored", joined.name);
            assert_eq!(joined.crap, None, "{} was scored", joined.name);
        }
        let sources = vec![
            "crates/alpha/src/lib.rs".to_string(),
            "crates/beta/src/lib.rs".to_string(),
        ];
        assert_eq!(
            result.attributions,
            vec![
                (
                    "crates/alpha/src/lib.rs".to_string(),
                    Attribution::Collision {
                        key: "src/lib.rs",
                        sources: sources.clone(),
                    }
                ),
                (
                    "crates/beta/src/lib.rs".to_string(),
                    Attribution::Collision {
                        key: "src/lib.rs",
                        sources,
                    }
                ),
            ]
        );
    }

    /// C19: the claims on a contested record are not always symmetric. One
    /// claimant whose path *is* the record's key has evidence the other, which
    /// merely shares a suffix with it, does not — so the exact claimant is
    /// scored and the suffix claimant is superseded (still diagnosed, still
    /// N/A). Before C19 both were N/A and a provable attribution was thrown
    /// away.
    #[test]
    fn a_single_exact_claimant_wins_a_contested_record() {
        let lcov = parse_lcov("SF:src/lib.rs\nDA:10,1\nend_of_record\n").expect("valid fixture");
        let units = vec![
            SourceUnit {
                module: "alpha".to_string(),
                path: "src/lib.rs".to_string(),
                functions: vec![fc("alpha_one", 2, 10, 13)],
            },
            SourceUnit {
                module: "beta".to_string(),
                path: "crates/beta/src/lib.rs".to_string(),
                functions: vec![fc("beta_one", 1, 10, 13)],
            },
        ];
        let result = join(&lcov, &units);

        assert_eq!(result.functions[0].coverage, Some(1.0));
        assert_eq!(result.functions[1].coverage, None);
        assert_eq!(result.functions[1].crap, None);
        assert_eq!(
            result.attributions,
            vec![
                (
                    "src/lib.rs".to_string(),
                    Attribution::Resolved(Resolution::Exact("src/lib.rs"))
                ),
                (
                    "crates/beta/src/lib.rs".to_string(),
                    Attribution::Superseded {
                        key: "src/lib.rs",
                        winner: "src/lib.rs".to_string(),
                    }
                ),
            ]
        );
    }

    /// The other side of C19: two exact claimants rank equally, so the record
    /// is still attributed to neither. Reachable because normalization maps
    /// distinct spellings onto one key — `./src/lib.rs` and `src/lib.rs` are
    /// two source files as far as the join is concerned.
    #[test]
    fn two_exact_claimants_stay_a_collision() {
        let lcov = parse_lcov("SF:src/lib.rs\nDA:10,1\nend_of_record\n").expect("valid fixture");
        let units = vec![
            SourceUnit {
                module: "alpha".to_string(),
                path: "src/lib.rs".to_string(),
                functions: vec![fc("alpha_one", 2, 10, 13)],
            },
            SourceUnit {
                module: "beta".to_string(),
                path: "./src/lib.rs".to_string(),
                functions: vec![fc("beta_one", 1, 10, 13)],
            },
        ];
        let result = join(&lcov, &units);

        for joined in &result.functions {
            assert_eq!(joined.coverage, None, "{} was scored", joined.name);
        }
        let sources = vec!["src/lib.rs".to_string(), "./src/lib.rs".to_string()];
        assert_eq!(
            result.attributions,
            vec![
                (
                    "src/lib.rs".to_string(),
                    Attribution::Collision {
                        key: "src/lib.rs",
                        sources: sources.clone(),
                    }
                ),
                (
                    "./src/lib.rs".to_string(),
                    Attribution::Collision {
                        key: "src/lib.rs",
                        sources,
                    }
                ),
            ]
        );
    }

    /// C20: `module` is the function's *full* module path, so an inline `mod`
    /// segment belongs in it — not glued to the function's name. Neither half
    /// can produce it alone: crate/file identity exists only upstream of the
    /// join, inline nesting only in the AST.
    #[test]
    fn inline_module_segments_extend_the_module_path() {
        let lcov = parse_lcov(FIXTURE).expect("valid fixture");
        let units = vec![SourceUnit {
            module: "demo::foo".to_string(),
            path: "src/foo.rs".to_string(),
            functions: vec![
                fc("top", 1, 10, 13),
                FunctionComplexity {
                    module_path: vec!["inner".to_string(), "deeper".to_string()],
                    ..fc("bar", 1, 10, 13)
                },
            ],
        }];
        let joined = join(&lcov, &units).functions;

        assert_eq!(joined[0].module, "demo::foo");
        assert_eq!(joined[0].name, "top");
        assert_eq!(joined[1].module, "demo::foo::inner::deeper");
        assert_eq!(joined[1].name, "bar");
    }

    /// The other half of defect 3: an uncontested record is still scored — the
    /// collision guard must not turn every suffix match into N/A.
    #[test]
    fn one_record_per_source_is_still_scored() {
        let lcov = parse_lcov(
            "SF:/build/crates/alpha/src/lib.rs\nDA:10,1\nend_of_record\n\
             SF:/build/crates/beta/src/lib.rs\nDA:10,0\nend_of_record\n",
        )
        .expect("valid fixture");
        let units = vec![
            SourceUnit {
                module: "alpha".to_string(),
                path: "crates/alpha/src/lib.rs".to_string(),
                functions: vec![fc("alpha_one", 1, 10, 13)],
            },
            SourceUnit {
                module: "beta".to_string(),
                path: "crates/beta/src/lib.rs".to_string(),
                functions: vec![fc("beta_one", 1, 10, 13)],
            },
        ];
        let result = join(&lcov, &units);

        // Each member's longest overlap is its own key, so no record is
        // contested and both suffix matches are still scored.
        assert_eq!(result.functions[0].coverage, Some(1.0));
        assert_eq!(result.functions[1].coverage, Some(0.0));
    }

    #[test]
    fn total_zero_is_one_c13_divergence() {
        // File present but the fn's range has no instrumented (DA) lines.
        // C13: total==0 ⇒ cov=1 (divergence from crap4go's 0.0).
        let j = join_one("src/lib.rs", fc("no_da", 9, 100, 110));
        assert_eq!(j.coverage, Some(1.0));
        assert_eq!(j.crap, crap::score(9, Some(1.0)));
    }

    #[test]
    fn path_normalization_suffix_match_resolves_coverage() {
        // LCOV key is `src/lib.rs`; each of these must resolve to it and find
        // the partial (0.5) coverage over lines 10..=13 — not None.
        for query in [
            "./src/lib.rs",         // leading ./
            "src\\lib.rs",          // Windows backslashes
            "/abs/proj/src/lib.rs", // absolute-style suffix
        ] {
            let j = join_one(query, fc("q", 2, 10, 13));
            assert_eq!(j.coverage, Some(0.5), "query {query} should resolve");
            assert!(j.crap.is_some(), "query {query} should be scored");
        }
    }

    #[test]
    fn produced_coverage_is_within_unit_interval() {
        let lcov = parse_lcov(FIXTURE).expect("valid fixture");
        let files = vec![unit(
            "src/lib.rs",
            vec![
                fc("partial", 2, 10, 13),
                fc("full", 4, 20, 21),
                fc("zero", 3, 12, 13),
                fc("no_da", 9, 100, 110),
            ],
        )];
        for j in join(&lcov, &files).functions {
            if let Some(cov) = j.coverage {
                assert!((0.0..=1.0).contains(&cov), "cov {cov} out of [0,1]");
                assert!(!cov.is_nan(), "cov is NaN");
            }
        }
    }

    #[test]
    fn attributions_report_one_status_per_file_in_input_order() {
        // FC-T5b: the join surfaces *how* each file's coverage was attributed,
        // one entry per source file (not per function), without printing.
        let lcov = parse_lcov(FIXTURE).expect("valid fixture");
        let files = vec![
            unit(
                "src/lib.rs",
                vec![fc("exact_a", 1, 10, 13), fc("exact_b", 1, 20, 21)],
            ),
            unit("crates/demo/src/other.rs", vec![fc("gone_a", 1, 10, 13)]),
            unit("src/other.rs", vec![fc("gone_b", 1, 10, 13)]),
        ];
        let result = join(&lcov, &files);

        assert_eq!(
            result.attributions,
            vec![
                (
                    "src/lib.rs".to_string(),
                    Attribution::Resolved(Resolution::Exact("src/lib.rs"))
                ),
                (
                    "crates/demo/src/other.rs".to_string(),
                    Attribution::Resolved(Resolution::Unresolved)
                ),
                (
                    "src/other.rs".to_string(),
                    Attribution::Resolved(Resolution::Unresolved)
                ),
            ]
        );
        // The two functions in the first file yield a single attribution entry.
        assert_eq!(result.functions.len(), 4);
    }

    /// End-to-end span seam: real `syn` spans from the CC engine flow through
    /// `usize→u32` narrowing into `coverage_in_range`. Unlike the unit tests
    /// above (which hand-build `FunctionComplexity` with manual line numbers),
    /// this drives `analyze_str` on inline source so the asserted coverage
    /// depends on genuine analyzed spans intersecting real `DA:` lines.
    #[test]
    fn integration_real_spans_join_against_lcov() {
        // Inline source; `\` keeps `fn simple` on line 1 (1-based, matching syn
        // spans). Line map used to build the LCOV fixture below:
        //   1: fn simple() -> i32 {
        //   2:     let mut n = 0;
        //   3:     n += 1;
        //   4:     n
        //   5: }
        //   6: (blank)
        //   7: fn branchy(x: i32) -> i32 {
        //   8:     match x {
        //   9:         0 => 0,
        //  10:         _ => 1,
        //  11:     }
        //  12: }
        const SRC: &str = "\
fn simple() -> i32 {
    let mut n = 0;
    n += 1;
    n
}

fn branchy(x: i32) -> i32 {
    match x {
        0 => 0,
        _ => 1,
    }
}
";
        let fns = crate::complexity::analyze_str(SRC).expect("SRC parses");
        let find = |name: &str| {
            fns.iter()
                .find(|f| f.name == name)
                .unwrap_or_else(|| panic!("no fn named {name}"))
        };
        let simple = find("simple");
        let branchy = find("branchy");

        // Self-documenting: assert the real analyzed spans the fixture relies on.
        assert_eq!((simple.start_line, simple.end_line), (1, 5));
        assert_eq!((branchy.start_line, branchy.end_line), (7, 12));
        // `match` with one literal arm (`_` exempt) ⇒ CC = base 1 + 1 = 2.
        assert_eq!(branchy.complexity, 2);
        assert_eq!(simple.complexity, 1);

        // DA lines fall inside the analyzed spans:
        //   simple  [1..=5]: lines 2,3 both hit          ⇒ 2/2 = 1.0
        //   branchy [7..=12]: 8,9 hit; 10,11 uncovered    ⇒ 2/4 = 0.5
        const LCOV: &str = "\
SF:src/demo.rs
DA:2,1
DA:3,4
DA:8,1
DA:9,2
DA:10,0
DA:11,0
end_of_record
";
        let lcov = parse_lcov(LCOV).expect("valid fixture");
        let joined = join(&lcov, &[unit("src/demo.rs", fns.clone())]).functions;

        let jf = |name: &str| {
            joined
                .iter()
                .find(|j| j.name == name)
                .unwrap_or_else(|| panic!("no joined fn named {name}"))
        };
        let jsimple = jf("simple");
        let jbranchy = jf("branchy");

        assert_eq!(jsimple.coverage, Some(1.0));
        assert_eq!(jsimple.crap, crap::score(simple.complexity, Some(1.0)));
        assert_eq!(jbranchy.coverage, Some(0.5));
        assert_eq!(jbranchy.crap, crap::score(branchy.complexity, Some(0.5)));
    }
}
