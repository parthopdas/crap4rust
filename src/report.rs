//! Table reporter (T6) — pure, I/O-free.
//!
//! Renders the human-readable "CRAP Report" table to a `String` per ruling
//! **C14** (source-verified against crap4go's `FormatReport`). It never touches
//! stdout, stderr, or the process exit code: the CLI (T7) owns those. That
//! separation is what makes the exact wire format assertable in unit tests
//! without capturing process output.
//!
//! **Why a separate [`ReportRow`] instead of formatting [`JoinedFunction`]
//! directly.** C14's second column is *Module*, an opaque display string the
//! reporter is **told**: crate-qualified module naming (C3) needs package
//! identity, which exists only in the discovery adapter, so that is where it is
//! derived (T9/FC-T6a — this is no longer a placeholder). [`rows_from_joined`]
//! is therefore a trivial projection, and [`format_report`] plus its locked
//! byte-for-byte output are the only formatting in the pipeline.
//!
//! **Layout parity notes.** Go's `%-30s`/`%-35s` do not truncate — an
//! over-long cell overflows and pushes the rest of the line right. Rust's
//! `{:<30}` behaves identically, so we simply match it (locked by
//! `long_cells_overflow_and_do_not_truncate`). Per FC-T6 the table is C14's
//! five columns only; `crap::band` is intentionally not wired in here.

use std::cmp::Ordering;

use crate::join::JoinedFunction;

/// Title line, and the `=` underline that follows it (C14).
const TITLE: &str = "CRAP Report";

/// One rendered row of the report: the display values C14 asks for, already
/// decoupled from how they were derived.
pub(crate) struct ReportRow {
    /// Qualified function display name (`Function` column).
    pub(crate) name: String,
    /// Crate-qualified module display string (`Module` column, C3). Supplied by
    /// the caller; the reporter never derives crate identity itself.
    pub(crate) module: String,
    /// Source file path — **not** rendered by C14's table. Half of the stable
    /// identity key (C15), read here only as the last-resort tiebreak that
    /// makes [`order_by_crap`] a *total* order.
    ///
    /// C14-display-only, and a **candidate for deletion**: [`ReportRow`] is the
    /// five-column display projection, and the JSON reporter already reads C15
    /// identity from [`JoinedFunction`] (it needs the file's attribution too,
    /// which a row structurally cannot carry). Deleting it means ordering the
    /// table over [`JoinedFunction`] as well — carried to T13 (FC-T11e).
    pub(crate) file: String,
    /// 1-based start line — **not** rendered by C14's table. Second half of the
    /// C15 identity key; same standing as [`file`](Self::file).
    pub(crate) start_line: usize,
    /// Cyclomatic complexity (`CC` column).
    pub(crate) complexity: u32,
    /// Coverage fraction in `[0.0, 1.0]`, or `None` ⇒ `N/A` (C13).
    pub(crate) coverage: Option<f64>,
    /// CRAP score, or `None` ⇒ `N/A` (sorts last).
    pub(crate) crap: Option<f64>,
}

/// Project joined functions onto report rows.
///
/// Purely a field-for-field projection: the Module string is derived upstream
/// (see the module docs), so nothing is computed here.
pub(crate) fn rows_from_joined(fns: &[JoinedFunction]) -> Vec<ReportRow> {
    fns.iter()
        .map(|f| ReportRow {
            name: f.name.clone(),
            module: f.module.clone(),
            file: f.file.clone(),
            start_line: f.start_line,
            complexity: f.complexity,
            coverage: f.coverage,
            crap: f.crap,
        })
        .collect()
}

/// Render the full report per C14, in [`order_by_crap`]'s total order.
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
    // The comparator is total (see `order_by_crap`), so the result does not
    // depend on the sort being stable — or on the input order.
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

/// C14 ordering: see [`order_by_crap`].
fn compare(a: &ReportRow, b: &ReportRow) -> Ordering {
    order_by_crap(
        OrderKey {
            crap: a.crap,
            name: &a.name,
            file: &a.file,
            start_line: a.start_line,
        },
        OrderKey {
            crap: b.crap,
            name: &b.name,
            file: &b.file,
            start_line: b.start_line,
        },
    )
}

/// Everything the report's ordering keys on, named rather than positional:
/// `name` and `file` are both `&str`, and a silent swap of the two would only
/// show up as a reordering of the frozen JSON contract.
pub(crate) struct OrderKey<'a> {
    pub(crate) crap: Option<f64>,
    pub(crate) name: &'a str,
    pub(crate) file: &'a str,
    pub(crate) start_line: usize,
}

/// The report's ordering (FC-T6d/FC-T9q): **CRAP descending, `None` last, then
/// `name` ascending, then `file` ascending, then `start_line` ascending.**
///
/// Hoisted out of [`compare`] so the JSON reporter orders its `functions[]` by
/// **this** definition rather than a copy of it: a second implementation would
/// let the two reporters disagree about the order of the same run, and in JSON
/// the order is part of a frozen contract.
///
/// **Total, deliberately.** `(crap, name)` alone is partial — rows agreeing on
/// both fell through to whatever order the caller happened to pass, which is
/// the module graph's BFS order, i.e. exactly the traversal detail FC-T9q
/// exists to keep out of the contract, hiding at the bottom of the tiebreak
/// chain. `file` + `start_line` is the C15 identity and therefore unique, so
/// the chain always terminates in a decision and the result no longer depends
/// on the sort algorithm being stable — a property an unstable parallel sort
/// would otherwise quietly destroy.
///
/// **C36 — a deliberate divergence from crap4go, in the same register as C16.**
/// Upstream's tie order is *not* arbitrary. At `unclebob/crap4go` @ `bee16db`,
/// `SortByCRAP` (`internal/crap/crap.go`) uses `sort.SliceStable`, and its
/// comparator returns `*a > *b` for two **scored** entries — `false` in both
/// directions when the scores are equal — so equal-CRAP rows keep their input
/// order, which `cmd/crap4go/main.go` fixes as `findSourceFiles`' `sort.Strings`
/// file order, then declaration order within each file. `Name` is consulted
/// **only** when both entries' CRAP is `nil`. Upstream's order is therefore
/// stable and knowable.
///
/// We have diverged from it since **T6**: our comparator breaks *scored* ties on
/// `name`, which upstream never consults for scored rows, and that divergence is
/// locked by an existing C14 test. Adding `file` and `start_line` does not
/// introduce it — it refines what **our own** comparator left undefined *below*
/// the pre-existing `name` tiebreak, replacing "whatever order the caller
/// passed" (module-graph BFS) with C15 identity. The result is strictly more
/// deterministic than both our previous behaviour and upstream's, and it no
/// longer depends on the sort being stable — which is what lets an unstable
/// parallel sort be used here safely.
pub(crate) fn order_by_crap(a: OrderKey<'_>, b: OrderKey<'_>) -> Ordering {
    let by_crap = match (a.crap, b.crap) {
        // `total_cmp` gives a total order without `partial_cmp` + unwrap.
        (Some(x), Some(y)) => y.total_cmp(&x),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    };
    by_crap
        .then_with(|| a.name.cmp(b.name))
        .then_with(|| a.file.cmp(b.file))
        .then_with(|| a.start_line.cmp(&b.start_line))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, module: &str, cc: u32, coverage: Option<f64>) -> ReportRow {
        ReportRow {
            name: name.to_string(),
            module: module.to_string(),
            file: module.to_string(),
            start_line: 1,
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

    /// **Locked (C14/C36) — do not "fix" this test.** It exercises the *full*
    /// ordering key: three rows with identical CRAP **and** identical `name`,
    /// separable only by `file` then `start_line`, fed in deliberately wrong
    /// input order. None of the other locked `report.rs` tests contains two rows
    /// sharing both CRAP and name, so none of them can catch a regression below
    /// the `name` tiebreak — this one exists to close that hole. It must keep
    /// passing unchanged when the reporter moves to an unstable parallel sort
    /// (T12): that is precisely the property it pins.
    #[test]
    fn equal_crap_and_name_break_by_file_then_start_line() {
        fn keyed_row(module: &str, file: &str, start_line: usize) -> ReportRow {
            ReportRow {
                name: "dup".to_string(),
                module: module.to_string(),
                file: file.to_string(),
                start_line,
                complexity: 2,
                coverage: Some(0.5),
                crap: crate::crap::score(2, Some(0.5)),
            }
        }

        let rows = vec![
            keyed_row("crate::c", "src/b.rs", 1),
            keyed_row("crate::a", "src/a.rs", 30),
            keyed_row("crate::b", "src/a.rs", 10),
        ];
        let expected = format!(
            "{PREAMBLE}\
dup                            crate::b                               2   50.0%      2.5
dup                            crate::a                               2   50.0%      2.5
dup                            crate::c                               2   50.0%      2.5
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
    fn rows_from_joined_carries_values_and_the_crate_qualified_module() {
        let joined = vec![JoinedFunction {
            name: "Foo::bar".to_string(),
            module: "demo::foo".to_string(),
            file: "crates/demo/src/foo.rs".to_string(),
            start_line: 12,
            complexity: 3,
            coverage: Some(0.5),
            lines: Some(crate::join::LineCounts {
                covered: 1,
                total: 2,
            }),
            crap: crate::crap::score(3, Some(0.5)),
        }];
        let rows = rows_from_joined(&joined);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Foo::bar");
        // C3: the Module column shows the crate-qualified module, not the path.
        assert_eq!(rows[0].module, "demo::foo");
        assert_eq!(rows[0].complexity, 3);
        assert_eq!(rows[0].coverage, Some(0.5));
        assert_eq!(rows[0].crap, crate::crap::score(3, Some(0.5)));
    }

    /// C15: the identity fields are carried through verbatim (`file`, plus
    /// `start_line`) alongside the display `module`.
    #[test]
    fn rows_from_joined_carries_c15_identity_fields() {
        let joined = vec![JoinedFunction {
            name: "Foo::bar".to_string(),
            module: "demo::foo".to_string(),
            file: "crates/demo/src/foo.rs".to_string(),
            start_line: 12,
            complexity: 3,
            coverage: Some(0.5),
            lines: Some(crate::join::LineCounts {
                covered: 1,
                total: 2,
            }),
            crap: crate::crap::score(3, Some(0.5)),
        }];
        let rows = rows_from_joined(&joined);
        assert_eq!(rows[0].file, "crates/demo/src/foo.rs");
        assert_eq!(rows[0].start_line, 12);
    }
}
