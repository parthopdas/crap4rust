//! Coverage (LCOV) adapter edge.
//!
//! Parses the LCOV coverage data that `cargo llvm-cov --lcov` emits into a
//! per-file map of source line → execution (hit) count. Only line coverage is
//! modelled (C9 / A2): we read `SF` (source-file) and `DA` (line-hit) records
//! and silently skip every other record type (`FN`, `FNDA`, `BRDA`, `LF`, …).
//!
//! **Parsing is pure** — [`parse_lcov`] works over a `&str` with no I/O. Only
//! the thin [`load`] wrapper touches the filesystem, keeping the adapter
//! unit-testable without on-disk fixtures.
//!
//! Source-file paths are stored **exactly as written** in the `SF` line; this
//! layer does not normalize or canonicalize them — path matching against `syn`
//! spans is the T5 join's job.
//!
//! Consumed by the coverage join (T5) and CLI wiring (T7).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// Errors from reading or parsing LCOV coverage data. These map to the CLI's
/// operational-error exit code 1 (C6) — e.g. "unparseable LCOV".
#[derive(Debug, Error)]
pub(crate) enum LcovError {
    /// A `DA` record that is not `DA:<line>,<hits>[,<checksum>]` with numeric
    /// line and hit fields. The offending line is captured for diagnostics.
    #[error("malformed DA record: {0:?}")]
    MalformedDa(String),
    /// A `DA` record appeared before any `SF` record opened a file section.
    #[error("DA record before any SF record")]
    DaBeforeSf,
    /// The LCOV file could not be read.
    #[error("failed to read LCOV file {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Parsed line-coverage data: source-file path → (line number → hit count).
///
/// Paths are the raw `SF` strings (not normalized). Line hit counts of 0 mean
/// the line is instrumented but uncovered; >0 means covered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LcovData {
    files: BTreeMap<String, BTreeMap<u32, u64>>,
}

/// How a source path resolved against the LCOV key space (FC-T5b).
///
/// The variants carry *how* a file was matched, not just the key, so callers
/// that must report the resolution (the CLI's stderr diagnostics) can tell an
/// exact hit from a merely plausible suffix hit, an unresolvable tie, or an
/// outright miss. The join only ever needs [`key`](Resolution::key).
///
/// [`Ambiguous`](Self::Ambiguous) carries its tied candidates, so this type is
/// deliberately **not `Copy`**: a warning that says only "ambiguous" without
/// naming what it was torn between is not actionable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Resolution<'a> {
    /// The query matched a stored key exactly (after normalization).
    Exact(&'a str),
    /// The query matched exactly one best (longest-overlap) key by segment-wise
    /// suffix — plausible, but not proof that it is the same file.
    Suffix(&'a str),
    /// Several stored keys match the query equally well (FC-T5a). Treated as
    /// unresolved — [`key`](Self::key) is `None` — because picking one of them
    /// would be arbitrary, and an arbitrary pick reports another file's
    /// coverage numbers under this file's name.
    Ambiguous(Vec<&'a str>),
    /// No stored key covers the query: the file is absent from the profile.
    Unresolved,
}

impl<'a> Resolution<'a> {
    /// The matching **stored** key, to be fed back into
    /// [`LcovData::coverage_in_range`]; `None` when there is no single
    /// defensible key (unresolved, or ambiguous).
    pub(crate) fn key(&self) -> Option<&'a str> {
        match self {
            Self::Exact(key) | Self::Suffix(key) => Some(key),
            Self::Ambiguous(_) | Self::Unresolved => None,
        }
    }
}

impl LcovData {
    /// Line-hit map for `file`, if the LCOV data contains a section for it.
    ///
    /// Test-only: the wired pipeline never needs a raw, unnormalized lookup —
    /// it goes through [`resolve_path`](Self::resolve_path) +
    /// [`coverage_in_range`](Self::coverage_in_range). It survives as the
    /// parser's unit-test assertion surface, so it is compiled out of
    /// production builds rather than annotated as dead.
    #[cfg(test)]
    fn file(&self, file: &str) -> Option<&BTreeMap<u32, u64>> {
        self.files.get(file)
    }

    /// Resolve a query path (a CC-engine source path) to the LCOV key that
    /// covers it, in the spirit of crap4go's `segmentsForFile`. Returns *how*
    /// it resolved (FC-T5b) — the matching **stored** key plus its exactness,
    /// or [`Resolution::Unresolved`] when the file is genuinely absent from the
    /// coverage profile.
    ///
    /// Matching is path-separator- and `./`-insensitive (see [`normalize_path`]):
    /// 1. exact match on normalized keys; else
    /// 2. segment-wise **suffix** match — the shorter path's `/`-segments are a
    ///    suffix of the longer's, **in either direction** (see
    ///    [`suffix_overlap`]), e.g. `src/lib.rs` resolves an absolute
    ///    `/abs/proj/src/lib.rs` key and vice-versa. Among the candidates the
    ///    **longest overlap** wins, and a genuine tie is
    ///    [`Resolution::Ambiguous`] (FC-T5a) rather than an arbitrary pick:
    ///    `src/lib.rs` exists in every workspace member, so "first key wins"
    ///    silently attributes one member's coverage to another's functions.
    ///
    /// Step 2's bidirectionality is a **deliberate divergence** from crap4go —
    /// see [`suffix_overlap`] for the verified upstream behaviour and the
    /// rationale. The two-pass shape (exact, then suffix) matches upstream's
    /// `segmentsForFile`. Candidate order is `BTreeMap` order, so both the
    /// winner and a reported tie are deterministic (FC-T6b).
    ///
    /// The raw key map stays private; the join owns this key space, so the
    /// normalization lives here rather than leaking the inner `BTreeMap`.
    pub(crate) fn resolve_path(&self, path: &str) -> Resolution<'_> {
        let query = normalize_path(path);
        // 1. Exact match on normalized keys (an exact hit wins over a partial
        //    suffix hit, which is why this pass is kept separate).
        for key in self.files.keys() {
            if normalize_path(key) == query {
                return Resolution::Exact(key.as_str());
            }
        }
        // 2. Segment-wise suffix match, best (longest) overlap wins.
        let query_segs = segments(&query);
        let mut best: Vec<&str> = Vec::new();
        let mut best_overlap = 0;
        for key in self.files.keys() {
            let key_norm = normalize_path(key);
            let Some(overlap) = suffix_overlap(&query_segs, &segments(&key_norm)) else {
                continue;
            };
            if overlap > best_overlap {
                best_overlap = overlap;
                best.clear();
            }
            if overlap == best_overlap {
                best.push(key.as_str());
            }
        }
        match best.len() {
            0 => Resolution::Unresolved,
            1 => Resolution::Suffix(best[0]),
            _ => Resolution::Ambiguous(best),
        }
    }

    /// Coverage over the inclusive line range `[start_line, end_line]` of `file`.
    ///
    /// Returns `(covered, total)` where `total` is the number of instrumented
    /// (`DA`-recorded) lines in the range and `covered` is how many of those had
    /// a hit count > 0. An unknown file, or a range with no instrumented lines,
    /// yields `(0, 0)`. This is the accessor the T5 join consumes to derive a
    /// function's fractional `cov` from its `syn` span.
    pub(crate) fn coverage_in_range(
        &self,
        file: &str,
        start_line: u32,
        end_line: u32,
    ) -> (u64, u64) {
        let Some(lines) = self.files.get(file) else {
            return (0, 0);
        };
        let mut covered = 0;
        let mut total = 0;
        for (_, &hits) in lines.range(start_line..=end_line) {
            total += 1;
            if hits > 0 {
                covered += 1;
            }
        }
        (covered, total)
    }
}

/// Parse LCOV text into per-file line-hit maps. Pure — no I/O.
///
/// Recognizes `SF:` (opens a file section), `DA:` (a line's hit count), and
/// `end_of_record` (closes the section); all other record types are skipped
/// silently. A malformed `DA` line or a `DA` before any `SF` is an error.
///
/// If the same line appears in more than one `DA` record within a section, the
/// **maximum** hit count wins (a line is "covered" if any record covered it).
pub(crate) fn parse_lcov(input: &str) -> Result<LcovData, LcovError> {
    let mut files: BTreeMap<String, BTreeMap<u32, u64>> = BTreeMap::new();
    // The currently-open `SF` section, held directly as `(path, line→hits)`.
    // It is merged into `files` on `end_of_record`, on the next `SF`, and at
    // EOF — so there is never a fallible re-lookup by key.
    let mut current: Option<(String, BTreeMap<u32, u64>)> = None;

    for line in input.lines() {
        if let Some(path) = line.strip_prefix("SF:") {
            merge_section(&mut files, current.take());
            // Reopening an `SF` path resumes its accumulated hits (merge, not
            // clobber): take any prior section back out to keep filling it.
            let lines = files.remove(path).unwrap_or_default();
            current = Some((path.to_string(), lines));
        } else if let Some(rest) = line.strip_prefix("DA:") {
            let (_, section) = current.as_mut().ok_or(LcovError::DaBeforeSf)?;
            let (line_no, hits) =
                parse_da(rest).ok_or_else(|| LcovError::MalformedDa(line.to_string()))?;
            section
                .entry(line_no)
                .and_modify(|h| *h = (*h).max(hits))
                .or_insert(hits);
        } else if line == "end_of_record" {
            merge_section(&mut files, current.take());
        }
        // Any other record type (FN, FNDA, BRDA, LF, TN, …) is ignored.
    }
    // Flush a section left open at EOF (LCOV without a trailing end_of_record).
    merge_section(&mut files, current.take());

    Ok(LcovData { files })
}

/// Merge an open section's `line→hits` map into `files`, taking the **maximum**
/// hit count per line so a reopened `SF` path accumulates rather than clobbers.
/// An empty section still creates (or preserves) the file's entry.
fn merge_section(
    files: &mut BTreeMap<String, BTreeMap<u32, u64>>,
    section: Option<(String, BTreeMap<u32, u64>)>,
) {
    let Some((path, lines)) = section else {
        return;
    };
    let target = files.entry(path).or_default();
    for (line_no, hits) in lines {
        target
            .entry(line_no)
            .and_modify(|h| *h = (*h).max(hits))
            .or_insert(hits);
    }
}

/// Read an LCOV file from `path` and parse it. The only I/O in this module.
pub(crate) fn load(path: &Path) -> Result<LcovData, LcovError> {
    let text = std::fs::read_to_string(path).map_err(|source| LcovError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    parse_lcov(&text)
}

/// Parse the payload of a `DA:` record — `<line>,<hits>[,<checksum>]` — into
/// `(line, hits)`. The optional checksum third field is ignored. Returns `None`
/// for any non-numeric or missing line/hit field.
fn parse_da(rest: &str) -> Option<(u32, u64)> {
    let mut parts = rest.splitn(3, ',');
    let line_no = parts.next()?.parse::<u32>().ok()?;
    let hits = parts.next()?.parse::<u64>().ok()?;
    Some((line_no, hits))
}

/// Normalize a path for cross-side matching: convert Windows separators
/// `\` → `/`, then strip a single leading `./`.
///
/// **Verified parity** with crap4go's `normalizePath`
/// (`internal/coverage/coverage.go`, `unclebob/crap4go` @ `bee16db`), which is
/// `strings.TrimPrefix(strings.ReplaceAll(path, "\\", "/"), "./")`.
fn normalize_path(path: &str) -> String {
    let slashed = path.replace('\\', "/");
    slashed.strip_prefix("./").unwrap_or(&slashed).to_string()
}

/// Split an (already-normalized) path into its non-empty `/`-segments.
fn segments(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
}

/// How many trailing segments two paths share, when one segment list is a
/// suffix of the other **in either direction**; `None` when neither is (and for
/// empty lists, which never match).
///
/// The overlap is what makes "longest wins" possible: a query matching a key on
/// four trailing segments is a far better guess than one matching on two
/// (FC-T5a).
///
/// **Deliberate divergence from crap4go — verified.** Upstream's `suffixMatch`
/// (`internal/coverage/coverage.go`, `unclebob/crap4go` @ `bee16db`) is
/// *one-directional*: it returns `false` as soon as
/// `len(suffixParts) > len(pathParts)`, so only the queried file path may be a
/// suffix of the profile key, never the reverse. We match both ways on purpose:
/// LCOV `SF:` keys are typically short and repo-relative while the CC engine
/// reports absolute paths, so the one-directional rule would fail to resolve
/// and report `N/A` for essentially every function. Upstream's `segmentsForFile`
/// also stops at the first matching candidate while ranging over a Go map — an
/// *arbitrary* one when several match, since that iteration order is randomized;
/// we take the longest and refuse to guess on a tie.
fn suffix_overlap(a: &[&str], b: &[&str]) -> Option<usize> {
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    (!short.is_empty() && long.ends_with(short)).then_some(short.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_two_sections_with_zero_hit_lines() {
        let lcov = "\
SF:src/a.rs
DA:1,3
DA:2,0
DA:4,7
end_of_record
SF:src/b.rs
DA:10,1
DA:11,0
end_of_record
";
        let data = parse_lcov(lcov).expect("valid LCOV");

        let a = data.file("src/a.rs").expect("section a");
        assert_eq!(a, &BTreeMap::from([(1, 3), (2, 0), (4, 7)]));

        let b = data.file("src/b.rs").expect("section b");
        assert_eq!(b, &BTreeMap::from([(10, 1), (11, 0)]));
    }

    #[test]
    fn da_with_checksum_third_field_parses() {
        let lcov = "SF:src/a.rs\nDA:5,2,f0a1b2c3\nend_of_record\n";
        let data = parse_lcov(lcov).expect("valid LCOV");
        assert_eq!(data.file("src/a.rs").unwrap().get(&5), Some(&2));
    }

    #[test]
    fn unknown_record_types_are_skipped() {
        let lcov = "\
TN:
SF:src/a.rs
FN:1,foo
FNDA:3,foo
FNF:1
FNH:1
BRDA:2,0,0,1
BRF:1
BRH:1
DA:1,3
LF:1
LH:1
end_of_record
";
        let data = parse_lcov(lcov).expect("unknown records skipped");
        assert_eq!(data.file("src/a.rs").unwrap(), &BTreeMap::from([(1, 3)]));
    }

    #[test]
    fn coverage_in_range_counts_partial_overlap() {
        // Instrumented lines: 5(hit), 6(0), 8(hit), 12(hit).
        let lcov = "\
SF:src/a.rs
DA:5,1
DA:6,0
DA:8,4
DA:12,2
end_of_record
";
        let data = parse_lcov(lcov).expect("valid LCOV");

        // Range 5..=8 overlaps lines 5,6,8: total=3, covered=5 and 8 => 2.
        assert_eq!(data.coverage_in_range("src/a.rs", 5, 8), (2, 3));
        // Range 6..=6 is a single uncovered line.
        assert_eq!(data.coverage_in_range("src/a.rs", 6, 6), (0, 1));
        // Range with no instrumented lines.
        assert_eq!(data.coverage_in_range("src/a.rs", 20, 30), (0, 0));
        // Unknown file.
        assert_eq!(data.coverage_in_range("src/missing.rs", 1, 10), (0, 0));
    }

    #[test]
    fn later_da_takes_max_hit_count() {
        let lcov = "SF:src/a.rs\nDA:1,0\nDA:1,5\nDA:1,2\nend_of_record\n";
        let data = parse_lcov(lcov).expect("valid LCOV");
        assert_eq!(data.file("src/a.rs").unwrap().get(&1), Some(&5));
    }

    #[test]
    fn malformed_da_non_numeric_is_error() {
        let err = parse_lcov("SF:src/a.rs\nDA:abc,1\nend_of_record\n").unwrap_err();
        assert!(matches!(err, LcovError::MalformedDa(_)));
    }

    #[test]
    fn malformed_da_missing_hits_is_error() {
        let err = parse_lcov("SF:src/a.rs\nDA:5\nend_of_record\n").unwrap_err();
        assert!(matches!(err, LcovError::MalformedDa(_)));
    }

    #[test]
    fn da_before_sf_is_error() {
        let err = parse_lcov("DA:1,3\n").unwrap_err();
        assert!(matches!(err, LcovError::DaBeforeSf));
    }

    #[test]
    fn crlf_line_endings_parse_identically() {
        let lcov = "SF:src/a.rs\r\nDA:1,3\r\nDA:2,0\r\nend_of_record\r\n";
        let data = parse_lcov(lcov).expect("valid LCOV");
        // No trailing `\r` corrupts the path key or the DA values.
        let a = data.file("src/a.rs").expect("section a");
        assert_eq!(a, &BTreeMap::from([(1, 3), (2, 0)]));
    }

    #[test]
    fn reopened_sf_section_merges_da_lines() {
        let lcov = "\
SF:src/a.rs
DA:1,3
end_of_record
SF:src/a.rs
DA:2,7
end_of_record
";
        let data = parse_lcov(lcov).expect("valid LCOV");
        // Both occurrences' lines are present (merge, not clobber).
        assert_eq!(
            data.file("src/a.rs").unwrap(),
            &BTreeMap::from([(1, 3), (2, 7)])
        );
    }

    #[test]
    fn end_of_record_resets_open_section() {
        // After end_of_record, a DA with no new SF has no open section.
        let err = parse_lcov("SF:src/a.rs\nDA:1,3\nend_of_record\nDA:2,4\n").unwrap_err();
        assert!(matches!(err, LcovError::DaBeforeSf));
    }

    #[test]
    fn resolve_path_reports_exact_match() {
        let data = parse_lcov("SF:src/a.rs\nDA:1,3\nend_of_record\n").expect("valid LCOV");
        // Byte-identical, and normalization-equivalent, queries are all exact.
        for query in ["src/a.rs", "./src/a.rs", "src\\a.rs"] {
            assert_eq!(
                data.resolve_path(query),
                Resolution::Exact("src/a.rs"),
                "query {query}"
            );
        }
    }

    #[test]
    fn resolve_path_reports_suffix_match_with_key() {
        let data =
            parse_lcov("SF:/abs/proj/src/a.rs\nDA:1,3\nend_of_record\n").expect("valid LCOV");
        // Shorter query is a segment-wise suffix of the stored key, and the
        // stored key is reported so the CLI can name it (FC-T5b).
        assert_eq!(
            data.resolve_path("src/a.rs"),
            Resolution::Suffix("/abs/proj/src/a.rs")
        );
    }

    #[test]
    fn resolve_path_reports_unresolved() {
        let data = parse_lcov("SF:src/a.rs\nDA:1,3\nend_of_record\n").expect("valid LCOV");
        assert_eq!(data.resolve_path("src/other.rs"), Resolution::Unresolved);
    }

    #[test]
    fn resolve_path_prefers_the_longest_overlapping_key() {
        // FC-T5a: both keys are segment-wise suffixes of the query, but only
        // one of them agrees on the member directory. The shorter candidate
        // sorts first, so "first key wins" would have picked it.
        let data = parse_lcov(
            "SF:alpha/src/lib.rs\nDA:1,3\nend_of_record\n\
             SF:crates/alpha/src/lib.rs\nDA:1,3\nend_of_record\n",
        )
        .expect("valid LCOV");
        assert_eq!(
            data.resolve_path("proj/crates/alpha/src/lib.rs"),
            Resolution::Suffix("crates/alpha/src/lib.rs")
        );
    }

    #[test]
    fn resolve_path_reports_an_equal_overlap_tie_as_ambiguous() {
        // FC-T5a: `src/lib.rs` exists in every workspace member, so a short
        // query overlaps each member's key equally well. Picking the first
        // would silently report beta's coverage for alpha's functions.
        let data = parse_lcov(
            "SF:crates/alpha/src/lib.rs\nDA:1,3\nend_of_record\n\
             SF:crates/beta/src/lib.rs\nDA:1,0\nend_of_record\n",
        )
        .expect("valid LCOV");
        assert_eq!(
            data.resolve_path("src/lib.rs"),
            Resolution::Ambiguous(vec!["crates/alpha/src/lib.rs", "crates/beta/src/lib.rs"])
        );
    }

    #[test]
    fn an_exact_match_beats_an_otherwise_ambiguous_tie() {
        // The tie only matters when nothing matched exactly; an exact key is
        // still the answer even though both members carry `src/lib.rs`.
        let data = parse_lcov(
            "SF:crates/alpha/src/lib.rs\nDA:1,3\nend_of_record\n\
             SF:crates/beta/src/lib.rs\nDA:1,0\nend_of_record\n",
        )
        .expect("valid LCOV");
        assert_eq!(
            data.resolve_path("crates/beta/src/lib.rs"),
            Resolution::Exact("crates/beta/src/lib.rs")
        );
    }

    #[test]
    fn resolution_key_exposes_the_stored_key() {
        assert_eq!(Resolution::Exact("src/a.rs").key(), Some("src/a.rs"));
        assert_eq!(Resolution::Suffix("src/a.rs").key(), Some("src/a.rs"));
        assert_eq!(Resolution::Unresolved.key(), None);
        // FC-T5a: an ambiguous match is unresolved for join purposes, so the
        // C13 join contract needs no new case — those functions report N/A.
        assert_eq!(
            Resolution::Ambiguous(vec!["a/src/lib.rs", "b/src/lib.rs"]).key(),
            None
        );
    }
}
