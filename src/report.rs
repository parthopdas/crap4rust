//! Table reporter (T6) — pure, I/O-free.
//!
//! Renders the human-readable "CRAP Report" table to a `String` per ruling
//! **C14** (source-verified against crap4go's `FormatReport`). It never touches
//! stdout, stderr, or the process exit code: the CLI (T7) owns those. That
//! separation is what makes the exact wire format assertable in unit tests
//! without capturing process output.
//!
//! **Why a separate [`ReportRow`] instead of formatting [`JoinedFunction`]
//! directly.** C14's second column is *Module*, but the join only carries a
//! source `file` path — crate-qualified module naming is C3/S3 (T9), not this
//! task. So the reporter is deliberately *told* the module string rather than
//! deriving crate identity itself: [`ReportRow::module`] is an opaque display
//! string. For S1 (single crate) [`rows_from_joined`] fills it via
//! [`module_from_path`], a normalized-file-path **placeholder**. When S3 lands
//! real crate-qualified names it swaps that one producer — [`format_report`]
//! and its locked byte-for-byte output do not change.
//!
//! **Layout parity notes.** Go's `%-30s`/`%-35s` do not truncate — an
//! over-long cell overflows and pushes the rest of the line right. Rust's
//! `{:<30}` behaves identically, so we simply match it (locked by
//! `long_cells_overflow_and_do_not_truncate`). Per FC-T6 the table is C14's
//! five columns only; `crap::band` is intentionally not wired in here.
//!
//! Reached only by tests until the CLI wires the pipeline (T7), so `dead_code`
//! is allowed here, consistent with `join.rs`. **Remove this `allow` when T7
//! wires the CLI.**
#![allow(dead_code)]

use std::cmp::Ordering;

use crate::join::JoinedFunction;

/// Title line, and the `=` underline that follows it (C14).
const TITLE: &str = "CRAP Report";

/// One rendered row of the report: the display values C14 asks for, already
/// decoupled from how they were derived.
pub(crate) struct ReportRow {
    /// Qualified function display name (`Function` column).
    pub(crate) name: String,
    /// Opaque module display string (`Module` column). Supplied by the caller;
    /// the reporter never derives crate identity itself (see module docs).
    pub(crate) module: String,
    /// Cyclomatic complexity (`CC` column).
    pub(crate) complexity: u32,
    /// Coverage fraction in `[0.0, 1.0]`, or `None` ⇒ `N/A` (C13).
    pub(crate) coverage: Option<f64>,
    /// CRAP score, or `None` ⇒ `N/A` (sorts last).
    pub(crate) crap: Option<f64>,
}

/// Adapt joined functions into report rows, deriving the S1 placeholder module
/// from each function's file path.
///
/// S3/T9 replaces this adapter (not [`format_report`]) with one that supplies
/// crate-qualified module names.
pub(crate) fn rows_from_joined(fns: &[JoinedFunction]) -> Vec<ReportRow> {
    fns.iter()
        .map(|f| ReportRow {
            name: f.name.clone(),
            module: module_from_path(&f.file),
            complexity: f.complexity,
            coverage: f.coverage,
            crap: f.crap,
        })
        .collect()
}

/// Render the full report per C14, sorted by CRAP descending (`None` last,
/// ties broken by name ascending).
///
/// Returns the whole report as a `String`, every line newline-terminated. The
/// input is borrowed and never mutated; sorting happens on a local index.
pub(crate) fn format_report(rows: &[ReportRow]) -> String {
    let header = format_line("Function", "Module", "CC", "Cov%", "CRAP");

    let mut out = String::new();
    out.push_str(TITLE);
    out.push('\n');
    out.push_str(&"=".repeat(TITLE.chars().count()));
    out.push('\n');
    out.push_str(&header);
    out.push('\n');
    out.push_str(&"-".repeat(header.chars().count()));
    out.push('\n');

    let mut ordered: Vec<&ReportRow> = rows.iter().collect();
    // `sort_by` is stable, so equal keys keep input order; the name tie-break
    // makes the result deterministic regardless.
    ordered.sort_by(|a, b| compare(a, b));

    for row in ordered {
        out.push_str(&format_line(
            &row.name,
            &row.module,
            &row.complexity.to_string(),
            &format_coverage(row.coverage),
            &format_crap(row.crap),
        ));
        out.push('\n');
    }
    out
}

/// C14 ordering: CRAP descending, `None` last, ties broken by name ascending.
fn compare(a: &ReportRow, b: &ReportRow) -> Ordering {
    let by_crap = match (a.crap, b.crap) {
        // `total_cmp` gives a total order without `partial_cmp` + unwrap.
        (Some(x), Some(y)) => y.total_cmp(&x),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    };
    by_crap.then_with(|| a.name.cmp(&b.name))
}

/// The single column layout of C14 — Go `%-30s %-35s %4s %7s %8s`.
fn format_line(name: &str, module: &str, cc: &str, coverage: &str, crap: &str) -> String {
    format!("{name:<30} {module:<35} {cc:>4} {coverage:>7} {crap:>8}")
}

/// Cov% cell: `%5.1f%%` (e.g. ` 87.5%`), or the 6-char `  N/A ` when absent.
fn format_coverage(coverage: Option<f64>) -> String {
    match coverage {
        Some(c) => format!("{:>5.1}%", c * 100.0),
        None => "  N/A ".to_string(),
    }
}

/// CRAP cell: `%8.1f`, or the 8-char `     N/A` when unscored.
fn format_crap(crap: Option<f64>) -> String {
    match crap {
        Some(c) => format!("{c:>8.1}"),
        None => "     N/A".to_string(),
    }
}

/// **S1 placeholder** module string: the source path, normalized to forward
/// slashes with any leading `./` removed.
///
/// Deliberately *not* a crate-qualified module name — that is C3/S3 (T9), which
/// needs package identity the join does not carry (FC-T5e). Showing the
/// normalized path keeps the column honest and obviously provisional instead of
/// inventing a `::` name S3 would have to undo.
fn module_from_path(file: &str) -> String {
    let normalized = file.replace('\\', "/");
    normalized
        .strip_prefix("./")
        .unwrap_or(&normalized)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, module: &str, cc: u32, coverage: Option<f64>) -> ReportRow {
        ReportRow {
            name: name.to_string(),
            module: module.to_string(),
            complexity: cc,
            coverage,
            crap: crate::crap::score(cc, coverage),
        }
    }

    /// The four fixed lines every report starts with, byte-for-byte.
    const PREAMBLE: &str = "\
CRAP Report
===========
Function                       Module                                CC    Cov%     CRAP
----------------------------------------------------------------------------------------
";

    #[test]
    fn empty_report_is_preamble_only() {
        assert_eq!(format_report(&[]), PREAMBLE);
    }

    #[test]
    fn header_and_separator_widths_match_c14() {
        let header = format_line("Function", "Module", "CC", "Cov%", "CRAP");
        // 30 + 1 + 35 + 1 + 4 + 1 + 7 + 1 + 8.
        assert_eq!(header.len(), 88);
        let report = format_report(&[]);
        let lines: Vec<&str> = report.lines().collect();
        assert_eq!(lines[0], "CRAP Report");
        assert_eq!(lines[1], "===========");
        assert_eq!(lines[2], header);
        assert_eq!(lines[3], "-".repeat(88));
    }

    #[test]
    fn mixed_coverage_rows_render_and_sort_by_crap_desc() {
        // CRAP: zero  = 4²·1     + 4 = 20.0
        //       part  = 3²·0.125 + 3 =  4.1 (4.125)
        //       full  =              5 =  5.0
        let rows = vec![
            row("part", "src/a.rs", 3, Some(0.5)),
            row("full", "src/b.rs", 5, Some(1.0)),
            row("zero", "src/c.rs", 4, Some(0.0)),
        ];
        let expected = format!(
            "{PREAMBLE}\
zero                           src/c.rs                               4    0.0%     20.0
full                           src/b.rs                               5  100.0%      5.0
part                           src/a.rs                               3   50.0%      4.1
"
        );
        assert_eq!(format_report(&rows), expected);
    }

    #[test]
    fn absent_coverage_renders_na_and_sorts_last() {
        let rows = vec![
            row("absent", "src/a.rs", 9, None),
            row("scored", "src/b.rs", 1, Some(1.0)),
        ];
        let expected = format!(
            "{PREAMBLE}\
scored                         src/b.rs                               1  100.0%      1.0
absent                         src/a.rs                               9    N/A       N/A
"
        );
        assert_eq!(format_report(&rows), expected);
    }

    #[test]
    fn equal_crap_ties_break_by_name_ascending() {
        // All three share CC=2, cov=0.5 ⇒ identical CRAP; input order is
        // deliberately reversed to prove the tie-break, not input stability.
        let rows = vec![
            row("charlie", "src/a.rs", 2, Some(0.5)),
            row("alpha", "src/a.rs", 2, Some(0.5)),
            row("bravo", "src/a.rs", 2, Some(0.5)),
        ];
        let names: Vec<String> = format_report(&rows)
            .lines()
            .skip(4)
            .map(|l| l.split_whitespace().collect::<Vec<_>>()[0].to_string())
            .collect();
        assert_eq!(names, vec!["alpha", "bravo", "charlie"]);
    }

    #[test]
    fn unscored_rows_also_tie_break_by_name_ascending() {
        let rows = vec![
            row("zulu", "src/a.rs", 2, None),
            row("alpha", "src/a.rs", 9, None),
        ];
        let expected = format!(
            "{PREAMBLE}\
alpha                          src/a.rs                               9    N/A       N/A
zulu                           src/a.rs                               2    N/A       N/A
"
        );
        assert_eq!(format_report(&rows), expected);
    }

    #[test]
    fn long_cells_overflow_and_do_not_truncate() {
        // Go's `%-30s`/`%-35s` never truncate; they overflow and push the rest
        // of the line right. We match that byte-for-byte.
        let rows = vec![row(
            "an_extremely_long_function_name_well_past_thirty",
            "crate::deeply::nested::module::path::that::exceeds::thirty_five",
            2,
            Some(0.5),
        )];
        let expected = format!(
            "{PREAMBLE}\
an_extremely_long_function_name_well_past_thirty crate::deeply::nested::module::path::that::exceeds::thirty_five    2   50.0%      2.5
"
        );
        assert_eq!(format_report(&rows), expected);
    }

    #[test]
    fn rows_from_joined_carries_values_and_placeholder_module() {
        let joined = vec![JoinedFunction {
            name: "Foo::bar".to_string(),
            file: "./src/foo.rs".to_string(),
            complexity: 3,
            coverage: Some(0.5),
            crap: crate::crap::score(3, Some(0.5)),
        }];
        let rows = rows_from_joined(&joined);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Foo::bar");
        assert_eq!(rows[0].module, "src/foo.rs");
        assert_eq!(rows[0].complexity, 3);
        assert_eq!(rows[0].coverage, Some(0.5));
        assert_eq!(rows[0].crap, crate::crap::score(3, Some(0.5)));
    }

    #[test]
    fn module_placeholder_normalizes_path() {
        assert_eq!(module_from_path("src/lib.rs"), "src/lib.rs");
        assert_eq!(module_from_path("./src/lib.rs"), "src/lib.rs");
        assert_eq!(module_from_path("src\\join.rs"), "src/join.rs");
    }
}
