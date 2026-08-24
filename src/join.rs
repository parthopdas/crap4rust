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
//!    mirrors crap4go's normalize + segment-suffix matching, and keep the
//!    file-absent vs file-present distinction all the way to the join contract.
//! 2. **The C13 join contract** — file-absent ⇒ `None` (N/A, unscored);
//!    file-present with no instrumented lines (`total==0`) ⇒ `Some(1.0)` (the
//!    deliberate divergence from crap4go); otherwise `Some(covered/total)`.
//!
//! Sorting, `N/A`-last ordering, the table columns (C14), and the "Module"
//! display string (C3/S3) are the reporter's job (T6/T7), not this module — we
//! only carry `file` through. Reached only by tests until the CLI wires the
//! pipeline (T7), so `dead_code` is allowed here, consistent with
//! `complexity.rs`/`coverage.rs`. **Remove this `allow` when T7 wires the CLI**
//! and the pipeline actually calls `join`.
#![allow(dead_code)]

use crate::complexity::FunctionComplexity;
use crate::coverage::LcovData;
use crate::crap;

/// One function after the coverage join: its complexity, resolved coverage
/// fraction (per the C13 contract), and CRAP score.
pub(crate) struct JoinedFunction {
    /// Qualified display name, carried straight from [`FunctionComplexity`].
    pub(crate) name: String,
    /// Source file path (as given to the join). Carried through for T6/T7
    /// "Module" derivation; not normalized here.
    pub(crate) file: String,
    /// Cyclomatic complexity.
    pub(crate) complexity: u32,
    /// Coverage fraction in `[0.0, 1.0]`, or `None` when the file is absent
    /// from the LCOV profile (N/A, unscored). Never NaN or out of range.
    pub(crate) coverage: Option<f64>,
    /// CRAP score = `crap::score(complexity, coverage)`; `None` mirrors an
    /// absent coverage.
    pub(crate) crap: Option<f64>,
}

/// Join per-file CC results against `lcov`, producing one [`JoinedFunction`]
/// per input function.
///
/// `files` pairs each source file path with the functions the CC engine found
/// in it — the association `FunctionComplexity` itself does not carry. Output
/// order follows the input; sorting/formatting is the reporter's job (C14).
pub(crate) fn join(
    lcov: &LcovData,
    files: &[(String, Vec<FunctionComplexity>)],
) -> Vec<JoinedFunction> {
    let mut out = Vec::new();
    for (file, fns) in files {
        // Resolve the file once per file, not per function.
        let resolved = lcov.resolve_path(file);
        for fc in fns {
            let coverage = coverage_for(lcov, resolved, fc);
            let crap = crap::score(fc.complexity, coverage);
            out.push(JoinedFunction {
                name: fc.name.clone(),
                file: file.clone(),
                complexity: fc.complexity,
                coverage,
                crap,
            });
        }
    }
    out
}

/// Apply the C13 contract for one function given its file's resolution result.
///
/// The returned `Some` fraction is always in `[0.0, 1.0]` and never NaN:
/// `covered ≤ total` by construction in [`LcovData::coverage_in_range`], and
/// the `total == 0` branch short-circuits before any division (FC-T4b).
fn coverage_for(lcov: &LcovData, resolved: Option<&str>, fc: &FunctionComplexity) -> Option<f64> {
    let key = resolved?; // file absent ⇒ None (N/A, unscored).
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
            complexity,
            start_line,
            end_line,
        }
    }

    fn join_one(file: &str, fc: FunctionComplexity) -> JoinedFunction {
        let lcov = parse_lcov(FIXTURE).expect("valid fixture");
        let mut out = join(&lcov, &[(file.to_string(), vec![fc])]);
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
        let files = vec![(
            "src/lib.rs".to_string(),
            vec![
                fc("partial", 2, 10, 13),
                fc("full", 4, 20, 21),
                fc("zero", 3, 12, 13),
                fc("no_da", 9, 100, 110),
            ],
        )];
        for j in join(&lcov, &files) {
            if let Some(cov) = j.coverage {
                assert!((0.0..=1.0).contains(&cov), "cov {cov} out of [0,1]");
                assert!(!cov.is_nan(), "cov is NaN");
            }
        }
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
        let joined = join(&lcov, &[("src/demo.rs".to_string(), fns.clone())]);

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
