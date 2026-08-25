//! JSON reporter (T11) — pure, I/O-free.
//!
//! Renders the versioned JSON document `--format json` emits. Like the table
//! reporter ([`crate::report`]) it returns a `String` and never touches a
//! stream: the CLI edge prints it, so the whole wire format is assertable in a
//! unit test.
//!
//! **This is the schema freeze.** Once `schema_version` is emitted, changing
//! the *shape* is a breaking change for every consumer, so the rulings the
//! document has to satisfy are stated here, next to the code that satisfies
//! them:
//!
//! * **Two sections, never merged (FC-T10d).** [`Request`] echoes what was
//!   *asked for*; [`Findings`] states what was *found*. The discriminator is
//!   "would this value differ if the same request produced a different
//!   artifact?" — if no, it is request echo. They are separate types so a
//!   *value* cannot drift from one section to the other unnoticed: a field is
//!   authored into one struct or the other, and moving it is a visible edit.
//!   That is all the type split buys. It does **not** make a *key collision*
//!   between the two a compile error — `#[serde(flatten)]` emits both copies
//!   and a decoder keeps the last — which is what the byte-locks and their
//!   top-level key-set assertion guard instead
//!   (`tests::the_document_shape_is_frozen` — `#[cfg(test)]`, so not
//!   linkable). They are `flatten`ed onto one object because C28 asks for a
//!   **top-level** `filters`, and the section boundary is a fact about the
//!   fields' meaning, not about how deeply they are nested.
//! * **The request echo exists at all (C28).** Without it a consumer cannot
//!   tell "no risk was found" from "you narrowed the report with `--filter`".
//! * **Explicit, total ordering (FC-T9q).** `functions[]` is sorted by C14's
//!   ordering ([`report::order_by_crap`]) — *by that function*, not a copy of
//!   it, and never by inheriting the module graph's BFS order, which would
//!   otherwise silently become part of the contract. The comparator is total
//!   (CRAP desc → `null` last → `name` → `file` → `start_line`), so the frozen
//!   order does not depend on the sort being stable.
//! * **Flat warnings (C24).** A `warnings[]` entry is
//!   `{code, severity, phase, file, line, column, message}` plus an
//!   **explicitly open, explicitly non-contractual** `data` object. A
//!   variant-shaped union would make every future `Kind` variant a schema
//!   change, and `Kind` went from 0 to 11 variants in two tasks. Consumers
//!   branch on `code` — append-only, never renamed — and read `message`;
//!   anything in `data` is best-effort.
//! * **Warning-only severity (C23).** `severity` is always `"warning"` while
//!   C6 holds. There is no error severity, because a diagnostic that says
//!   "error" next to exit code 0 is worse than no severity field at all;
//!   operational failures are an `Err`, and this document is never produced
//!   for one.
//! * **`declined_modules` only (FC-T10e).** `functions.length` *is* the row
//!   count, so no second count is emitted beside it — a redundant count is a
//!   second source of truth that can disagree with the first.
//! * **Not referentially closed (FC-T10c).** See [`data`].

use std::collections::BTreeMap;

use anyhow::Context;
use serde::Serialize;
use serde_json::{json, Value};

use crate::diagnostic::{Diagnostic, Kind, Phase};
use crate::join::{JoinedFile, JoinedFunction};
use crate::report;

/// The contract version this reporter emits.
///
/// An **integer**, not a string: it is a single monotonic counter, and every
/// consumer check is `== 1` or `>= 1`. A string invites `"1.0.0"`, which
/// invites semver range parsing, which invites a debate about what a "minor"
/// schema change is — and we have already ruled that additive `code` strings
/// and `data` keys are *not* schema changes at all. One number, bumped only
/// when a consumer that worked before would break.
///
/// # Compatibility policy
///
/// `schema_version` bumps only when a document valid under v1 would stop being
/// produced or would change meaning. Adding a top-level key, a
/// `warnings[].code`, a `data` key, a `coverage_caveat` tag, a
/// `coverage_source` value, or a `functions[]` field is **additive and does not
/// bump**. Consumers must ignore unknown keys and unknown enum values, and must
/// not assume a closed set for `code`, `coverage_caveat`, or `coverage_source`.
/// Removing or renaming anything, or changing a field's type or meaning, bumps.
/// The **order** of `functions[]` is part of the contract; changing the
/// ordering key bumps. That key is [`report::order_by_crap`] (C14/C36), called
/// here rather than copied, so the policy and the comparator cannot drift
/// apart.
const SCHEMA_VERSION: u32 = 1;

/// Where the run's coverage profile came from — request echo (C28/FC-T10d):
/// it is decided by the arguments, so it is the same whatever the artifact
/// turns out to say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CoverageSourceEcho {
    /// `--lcov-path`: the user brought their own profile and no coverage
    /// command was run.
    Provided,
    /// Zero-config (C2): a coverage command produced the profile this run.
    Generated,
}

/// The whole document.
#[derive(Serialize)]
struct Document<'a> {
    schema_version: u32,
    #[serde(flatten)]
    request: Request<'a>,
    #[serde(flatten)]
    findings: Findings<'a>,
}

/// What was asked for (C28) — never what was found (FC-T10d).
#[derive(Serialize)]
struct Request<'a> {
    /// The version of the tool that produced this document.
    ///
    /// Request echo under FC-T10d's discriminator — *would this value differ if
    /// the same request produced a different artifact?* No: it is fixed by the
    /// binary the request was made of, and reads the same whatever the run
    /// finds. `schema_version` describes the document's *shape*; this describes
    /// the *producer*, and nothing else does. Our CC rules (C1) and suffix
    /// heuristics (C16) will evolve, so a consumer diffing two runs across an
    /// upgrade cannot otherwise tell whether the numbers moved because the code
    /// changed or because the tool did.
    crap4rust_version: &'static str,
    /// The `--filter` fragments, as spelled, in the order given. Empty means
    /// the whole workspace was reported.
    ///
    /// A filter narrows the *report*, never the analysis (C26), so no number
    /// in [`Findings`] changes because of it — only which rows survive.
    filters: Vec<&'a str>,
    /// Whether the coverage profile was provided or generated this run.
    coverage_source: CoverageSourceEcho,
}

/// What was found — never what was asked for (FC-T10d).
#[derive(Serialize)]
struct Findings<'a> {
    /// Every reported function, sorted by C14's ordering (FC-T9q).
    functions: Vec<Function<'a>>,
    /// How many **distinct modules** discovery declined to analyse (C18/C22).
    ///
    /// Named for its unit: `"declined": 1` next to `"functions": [...]` would
    /// have read as one declined *function*, with the unit unrecoverable from
    /// the document; the explicit `_modules` suffix is what removes that
    /// reading.
    ///
    /// A **lower bound**, and structurally so: one declined `cfg_attr` module
    /// drops its whole subtree, whose size is unknown precisely because it was
    /// never walked. A consumer aggregating workspace CRAP from `functions[]`
    /// is measuring an incomplete workspace whenever this is non-zero, and
    /// cannot learn *how* incomplete from this document. It stays a bare
    /// integer: `warnings[]` already names *which* modules were declined, which
    /// is strictly more than an exactness flag would say.
    ///
    /// There is deliberately no row count beside it (FC-T10e):
    /// `functions.length` is that fact.
    declined_modules: usize,
    /// Everything the run had to say, flat (C24). Also written to stderr — the
    /// stream and the artifact are different channels, and a redirected
    /// artifact must be complete on its own.
    warnings: Vec<Warning<'a>>,
}

/// One reported function.
#[derive(Serialize)]
struct Function<'a> {
    /// Bare (or receiver-qualified) function name — `bar`, `Foo::bar`
    /// (C20/FC-T9a). Not unique: use `file` + `start_line` for identity (C15).
    name: &'a str,
    /// Full module path: crate, file scope, and any enclosing inline `mod`
    /// segments — `demo::foo::inner` (C3/C20/FC-T9a).
    module: &'a str,
    /// Workspace-relative, forward-slashed source path (FC-T7a). With
    /// `start_line` it is this row's identity (C15), and the identity every
    /// cross-reference in `data` uses (FC-T9o).
    file: &'a str,
    /// 1-based first line of the function.
    start_line: usize,
    /// Cyclomatic complexity.
    complexity: u32,
    /// Line-coverage fraction in `[0.0, 1.0]`, or `null` when the file's
    /// coverage could not be attributed (C13). `total == 0` is `1.0`, not
    /// `null`: a file with nothing instrumented has nothing untested.
    ///
    /// Unrounded — the table's one decimal place is a display choice.
    coverage: Option<f64>,
    /// The instrumented lines under this function that were hit, and how many
    /// there were — the exact numbers `coverage` is the ratio of, carried from
    /// the join rather than recomputed here (FC-T10e).
    ///
    /// They exist because the ratio alone is lossy in two ways a consumer
    /// cannot repair: C13's `total == 0 ⇒ 1.0` is **invisible** in a bare
    /// `1.0`, and coverage **cannot be aggregated at all** from ratios —
    /// averaging per-function fractions is arithmetically wrong and the
    /// document carried no weight to do it properly.
    ///
    /// Both are `null` exactly when `coverage` is `null` (the file-absent,
    /// ambiguous, contested and superseded cases): nothing was counted, which
    /// is a different fact from having counted zero lines, and `0` would assert
    /// the latter.
    covered_lines: Option<u64>,
    total_lines: Option<u64>,
    /// `cc² × (1 − cov)³ + cc`, or `null` exactly when `coverage` is `null`.
    crap: Option<f64>,
    /// Why this row's coverage is not simply trustworthy, or `null` when it is
    /// (FC-T9b\*): `"suffix"`, `"absent"`, `"ambiguous"`, `"contested"`,
    /// `"superseded"`. `"suffix"` is the dangerous one — the row **is** scored,
    /// but from a profile record matched only by path suffix, so the numbers
    /// may be another file's. The matching `warnings[]` entry carries the
    /// particulars.
    coverage_caveat: Option<&'static str>,
}

/// One warning, flat (C24).
#[derive(Serialize)]
struct Warning<'a> {
    /// Stable identifier to branch on. Append-only, never renamed.
    code: &'static str,
    /// Always `"warning"` while C6 holds (C23).
    severity: String,
    /// `"config"` (known before the run started) or `"run"` (FC-T8g).
    phase: &'static str,
    /// The file it is about, or `null` when it is about the run as a whole.
    file: Option<&'a str>,
    /// 1-based line/column when it points at a declaration rather than a file.
    line: Option<usize>,
    column: Option<usize>,
    /// The rendered human-readable text — the same words stderr shows, without
    /// the severity and site prefixes.
    message: String,
    /// The particulars, **open and non-contractual** (C24). Keys may be added,
    /// removed or reshaped for any `code` without a `schema_version` bump.
    /// Read it best-effort; branch on `code` and show `message`.
    data: BTreeMap<&'static str, Value>,
}

/// Render the JSON document (newline-terminated, like the table).
///
/// `files` is the join's output *after* `--filter` selection; `warnings` is
/// everything the invocation has said, config phase first.
pub(crate) fn render(
    files: &[JoinedFile<'_>],
    filters: Vec<&str>,
    coverage_source: CoverageSourceEcho,
    declined_modules: usize,
    warnings: &[&Diagnostic],
) -> anyhow::Result<String> {
    let document = Document {
        schema_version: SCHEMA_VERSION,
        request: Request {
            crap4rust_version: env!("CARGO_PKG_VERSION"),
            filters,
            coverage_source,
        },
        findings: Findings {
            functions: functions(files),
            declined_modules,
            warnings: warnings.iter().copied().map(warning).collect(),
        },
    };
    let mut rendered =
        serde_json::to_string_pretty(&document).context("failed to render the JSON report")?;
    rendered.push('\n');
    Ok(rendered)
}

/// Every function, **explicitly** ordered by C14's ordering (FC-T9q).
///
/// The input order here is the module graph's BFS order. Emitting it as-is
/// would freeze a traversal detail into the schema, so the sort is done here
/// and by [`report::order_by_crap`] — the table's own ordering function, not a
/// copy — so the two reporters can never disagree about one run's order.
///
/// Each row's caveat comes from the file it sits under, which is why the join
/// hands over [`JoinedFile`]s: nothing is re-associated by path string
/// (FC-T9b\*).
fn functions<'a>(files: &'a [JoinedFile<'_>]) -> Vec<Function<'a>> {
    let mut functions: Vec<Function<'a>> = files
        .iter()
        .flat_map(|file| {
            let caveat = file.attribution.caveat().map(|caveat| caveat.tag());
            file.functions
                .iter()
                .map(move |function| self::function(function, caveat))
        })
        .collect();
    functions.sort_by(|a, b| {
        report::order_by_crap(
            report::OrderKey {
                crap: a.crap,
                name: a.name,
                file: a.file,
                start_line: a.start_line,
            },
            report::OrderKey {
                crap: b.crap,
                name: b.name,
                file: b.file,
                start_line: b.start_line,
            },
        )
    });
    functions
}

fn function<'a>(
    function: &'a JoinedFunction,
    coverage_caveat: Option<&'static str>,
) -> Function<'a> {
    Function {
        name: &function.name,
        module: &function.module,
        file: &function.file,
        start_line: function.start_line,
        complexity: function.complexity,
        coverage: function.coverage,
        covered_lines: function.lines.map(|lines| lines.covered),
        total_lines: function.lines.map(|lines| lines.total),
        crap: function.crap,
        coverage_caveat,
    }
}

fn warning(diagnostic: &Diagnostic) -> Warning<'_> {
    let site = diagnostic.site.as_ref();
    Warning {
        code: diagnostic.code,
        // Rendered through `Display`, so the word has one source (C23).
        severity: diagnostic.severity.to_string(),
        phase: match diagnostic.phase {
            Phase::Config => "config",
            Phase::Run => "run",
        },
        file: site.map(|site| site.file.as_str()),
        line: site.and_then(|site| site.line),
        column: site.and_then(|site| site.column),
        message: diagnostic.kind.to_string(),
        data: data(&diagnostic.kind),
    }
}

/// The particulars of one diagnostic, as an open object (C24).
///
/// Every variant is listed rather than defaulted, so a new [`Kind`] is a
/// deliberate decision about what it exposes — but nothing here is
/// contractual: `code` and `message` are the contract, and these keys exist so
/// a consumer does not have to parse prose to act.
///
/// **The path-valued keys are not referentially closed** (FC-T10c).
/// `winner`, `claimants` and `analysed` are C15 identities — byte-identical to
/// the `file` those source files' rows carry (FC-T9o) — but attribution is
/// settled over the **whole workspace** (C26) while `functions[]` is narrowed
/// by `--filter`, so a named file may have no row in this document. That is
/// correct, not a defect: the alternative is either a lie about attribution or
/// numbers that change when you narrow the report. Resolve them as paths, not
/// as foreign keys.
fn data(kind: &Kind) -> BTreeMap<&'static str, Value> {
    match kind {
        Kind::IgnoredTestCommand | Kind::CoverageAbsent => BTreeMap::new(),
        Kind::TestCommandWithoutLcovToken { token, artifact } => {
            BTreeMap::from([("token", json!(token)), ("artifact", json!(artifact))])
        }
        Kind::ModuleFileMissing { module, candidates } => {
            BTreeMap::from([("module", json!(module)), ("candidates", json!(candidates))])
        }
        Kind::ModuleFileAmbiguous {
            module,
            candidates,
            analysed,
        } => BTreeMap::from([
            ("module", json!(module)),
            ("candidates", json!(candidates)),
            ("analysed", json!(analysed)),
        ]),
        Kind::ModulePathConditional { module, attribute } => {
            BTreeMap::from([("module", json!(module)), ("attribute", json!(attribute))])
        }
        Kind::CoverageSuffixMatch { key } => BTreeMap::from([("key", json!(key))]),
        Kind::CoverageAmbiguous { keys } => BTreeMap::from([("keys", json!(keys))]),
        Kind::CoverageContested { key, claimants } => {
            BTreeMap::from([("key", json!(key)), ("claimants", json!(claimants))])
        }
        Kind::CoverageSuperseded { key, winner } => {
            BTreeMap::from([("key", json!(key)), ("winner", json!(winner))])
        }
        Kind::FilterMatchedNothing { filter } => BTreeMap::from([("filter", json!(filter))]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::coverage::Resolution;
    use crate::diagnostic::Site;
    use crate::join::{Attribution, LineCounts};

    /// A scored function whose counts agree with its fraction: the fixtures use
    /// quarters, so `cov` is exactly `covered / 4`.
    fn function(
        name: &str,
        file: &str,
        start_line: usize,
        cc: u32,
        cov: Option<f64>,
    ) -> JoinedFunction {
        lined(
            name,
            file,
            start_line,
            cc,
            cov.map(|c| LineCounts {
                covered: (c * 4.0) as u64,
                total: 4,
            }),
        )
    }

    /// A function with its line counts stated outright — for the cases where
    /// the counts, not the fraction, are the point.
    fn lined(
        name: &str,
        file: &str,
        start_line: usize,
        cc: u32,
        lines: Option<LineCounts>,
    ) -> JoinedFunction {
        let cov = lines.map(LineCounts::fraction);
        JoinedFunction {
            name: name.to_string(),
            module: "demo".to_string(),
            file: file.to_string(),
            start_line,
            complexity: cc,
            coverage: cov,
            lines,
            crap: crate::crap::score(cc, cov),
        }
    }

    fn file<'a>(
        path: &str,
        attribution: Attribution<'a>,
        fns: Vec<JoinedFunction>,
    ) -> JoinedFile<'a> {
        JoinedFile {
            file: path.to_string(),
            attribution,
            functions: fns,
        }
    }

    fn exact(path: &'static str) -> Attribution<'static> {
        Attribution::Resolved(Resolution::Exact(path))
    }

    fn parsed(document: &str) -> Value {
        serde_json::from_str(document).expect("document is JSON")
    }

    /// The document's top-level keys **as parsed**, i.e. sorted: `serde_json`'s
    /// map is a `BTreeMap`, so this is the key *set*. The order the keys are
    /// *emitted* in is locked by [`the_document_shape_is_frozen`]'s byte
    /// assertion.
    const TOP_LEVEL_KEYS: [&str; 7] = [
        "coverage_source",
        "crap4rust_version",
        "declined_modules",
        "filters",
        "functions",
        "schema_version",
        "warnings",
    ];

    fn top_level_keys(document: &Value) -> Vec<&str> {
        document
            .as_object()
            .expect("document is an object")
            .keys()
            .map(String::as_str)
            .collect()
    }

    /// FC-T9q: the array is sorted by C14's ordering, never by input order.
    /// The fixture's input order is deliberately *not* its output order — the
    /// join hands over module-graph (BFS) order, and inheriting it would make
    /// a traversal detail part of the schema.
    #[test]
    fn functions_are_sorted_by_crap_descending_not_by_input_order() {
        let files = [file(
            "src/lib.rs",
            exact("src/lib.rs"),
            vec![
                function("low", "src/lib.rs", 1, 1, Some(1.0)),
                function("absent", "src/lib.rs", 20, 9, None),
                function("high", "src/lib.rs", 10, 6, Some(0.0)),
            ],
        )];

        let document =
            render(&files, Vec::new(), CoverageSourceEcho::Provided, 0, &[]).expect("render");
        let parsed = parsed(&document);
        let names: Vec<&str> = parsed["functions"]
            .as_array()
            .expect("functions is an array")
            .iter()
            .map(|f| f["name"].as_str().expect("name is a string"))
            .collect();

        // CRAP: high 42.0, low 1.0, absent N/A (last).
        assert_eq!(names, ["high", "low", "absent"]);
    }

    /// The full shape, byte-for-byte, for a small fixture: this is the freeze.
    ///
    /// The byte assertion locks the keys *as emitted*; the key-set assertion
    /// after it locks how many survive parsing, which is the only mechanical
    /// guard against a **key collision** between [`Request`] and [`Findings`].
    /// `#[serde(flatten)]` does not make a shared name a compile error: serde
    /// emits both copies and a decoder keeps the last, so a collision shows up
    /// here as six surviving keys — and a key added to either struct without a
    /// C31 decision as eight.
    #[test]
    fn the_document_shape_is_frozen() {
        let files = [file(
            "src/lib.rs",
            Attribution::Resolved(Resolution::Suffix("crates/demo/src/lib.rs")),
            vec![function("risky", "src/lib.rs", 5, 2, Some(0.5))],
        )];
        let diagnostic = Diagnostic::run(Site::file("src/lib.rs"), Kind::CoverageAbsent);

        let document = render(
            &files,
            vec!["crates/demo"],
            CoverageSourceEcho::Generated,
            1,
            &[&diagnostic],
        )
        .expect("render");

        assert_eq!(
            document,
            format!(
                r#"{{
  "schema_version": 1,
  "crap4rust_version": "{}",
  "filters": [
    "crates/demo"
  ],
  "coverage_source": "generated",
  "functions": [
    {{
      "name": "risky",
      "module": "demo",
      "file": "src/lib.rs",
      "start_line": 5,
      "complexity": 2,
      "coverage": 0.5,
      "covered_lines": 2,
      "total_lines": 4,
      "crap": 2.5,
      "coverage_caveat": "suffix"
    }}
  ],
  "declined_modules": 1,
  "warnings": [
    {{
      "code": "coverage-absent",
      "severity": "warning",
      "phase": "run",
      "file": "src/lib.rs",
      "line": null,
      "column": null,
      "message": "absent from the coverage profile; its functions report N/A",
      "data": {{}}
    }}
  ]
}}
"#,
                env!("CARGO_PKG_VERSION")
            )
        );
        assert_eq!(top_level_keys(&parsed(&document)), TOP_LEVEL_KEYS);
    }

    /// An **integral** CRAP and coverage render as `30.0` / `1.0`, never `30` /
    /// `1`. The distinction is invisible in Rust and fatal in a typed decoder:
    /// a Go `int64` or a strict TypeScript number type breaks on whichever it
    /// did not expect. Pinned here rather than left riding on `serde_json`'s
    /// float formatting.
    #[test]
    fn integral_floats_render_with_a_decimal_point() {
        // cov = 1.0 ⇒ crap = cc = 30.
        let files = [file(
            "src/lib.rs",
            exact("src/lib.rs"),
            vec![function("round", "src/lib.rs", 1, 30, Some(1.0))],
        )];

        let document =
            render(&files, Vec::new(), CoverageSourceEcho::Provided, 0, &[]).expect("render");

        assert!(document.contains("\"coverage\": 1.0,"), "{document}");
        assert!(document.contains("\"crap\": 30.0,"), "{document}");
    }

    /// C13, made visible (A5): a file that *is* in the profile with nothing
    /// instrumented under the function scores `1.0` — and the counts say why.
    /// Without them `1.0` is indistinguishable from a genuinely covered
    /// function, and no consumer can aggregate.
    #[test]
    fn nothing_instrumented_is_full_coverage_over_zero_lines() {
        let files = [file(
            "src/lib.rs",
            exact("src/lib.rs"),
            vec![lined(
                "uninstrumented",
                "src/lib.rs",
                1,
                4,
                Some(LineCounts {
                    covered: 0,
                    total: 0,
                }),
            )],
        )];

        let row =
            parsed(&render(&files, Vec::new(), CoverageSourceEcho::Provided, 0, &[]).unwrap())
                ["functions"][0]
                .clone();

        assert_eq!(row["coverage"], json!(1.0));
        assert_eq!(row["covered_lines"], json!(0));
        assert_eq!(row["total_lines"], json!(0));
    }

    /// The file-absent case: no ratio and no counts. `0` would claim we counted
    /// zero instrumented lines, which is a different — and false — statement.
    #[test]
    fn an_unattributed_row_has_no_ratio_and_no_counts() {
        let files = [file(
            "src/lib.rs",
            Attribution::Resolved(Resolution::Unresolved),
            vec![lined("absent", "src/lib.rs", 1, 4, None)],
        )];

        let row =
            parsed(&render(&files, Vec::new(), CoverageSourceEcho::Provided, 0, &[]).unwrap())
                ["functions"][0]
                .clone();

        assert_eq!(row["coverage"], Value::Null);
        assert_eq!(row["crap"], Value::Null);
        assert_eq!(row["covered_lines"], Value::Null);
        assert_eq!(row["total_lines"], Value::Null);
    }

    /// The ordering is **total** (A1): rows agreeing on CRAP *and* name are
    /// still ordered — by `file`, then `start_line`, the C15 identity — and not
    /// left in the module graph's BFS order at the bottom of the tiebreak
    /// chain. The fixture's input order is deliberately the reverse of the
    /// expected one, so a partial comparator (or an unstable sort) fails here.
    #[test]
    fn rows_with_equal_crap_and_equal_name_are_ordered_by_c15_identity() {
        let files = [
            file(
                "src/z.rs",
                exact("src/z.rs"),
                vec![
                    function("new", "src/z.rs", 20, 2, Some(0.5)),
                    function("new", "src/z.rs", 10, 2, Some(0.5)),
                ],
            ),
            file(
                "src/a.rs",
                exact("src/a.rs"),
                vec![function("new", "src/a.rs", 30, 2, Some(0.5))],
            ),
        ];

        let document =
            render(&files, Vec::new(), CoverageSourceEcho::Provided, 0, &[]).expect("render");
        let parsed = parsed(&document);
        let identity: Vec<(&str, u64)> = parsed["functions"]
            .as_array()
            .expect("functions is an array")
            .iter()
            .map(|f| {
                (
                    f["file"].as_str().expect("file is a string"),
                    f["start_line"].as_u64().expect("start_line is a number"),
                )
            })
            .collect();

        assert_eq!(
            identity,
            [("src/a.rs", 30), ("src/z.rs", 10), ("src/z.rs", 20)]
        );
    }

    /// C27's JSON analogue: "empty because everything was declined". A document
    /// with no rows still says why — the warnings name the modules and
    /// `declined_modules` counts them — so an empty `functions[]` is never
    /// mistaken for a clean workspace.
    #[test]
    fn an_empty_report_still_describes_why_it_is_empty() {
        let diagnostic = Diagnostic::run(
            Site::file("src/lib.rs"),
            Kind::ModulePathConditional {
                module: "demo::imp".to_string(),
                attribute: "#[cfg_attr(windows, path = \"windows.rs\")]".to_string(),
            },
        );

        let document = parsed(
            &render(
                &[],
                Vec::new(),
                CoverageSourceEcho::Generated,
                1,
                &[&diagnostic],
            )
            .expect("render"),
        );

        assert_eq!(document["functions"], json!([]));
        assert_eq!(document["declined_modules"], json!(1));
        assert_eq!(
            document["warnings"][0]["code"],
            json!("module-path-conditional")
        );
    }

    /// FC-T9b\*: the five caveat states reach the rows, and an exact match is
    /// `null` rather than a sixth tag. The tags come from the *attribution* the
    /// rows sit under — never from a path-string re-join.
    #[test]
    fn each_row_carries_its_files_caveat() {
        let files = [
            file(
                "a.rs",
                exact("a.rs"),
                vec![function("a", "a.rs", 1, 1, Some(1.0))],
            ),
            file(
                "b.rs",
                Attribution::Resolved(Resolution::Suffix("x/b.rs")),
                vec![function("b", "b.rs", 1, 1, Some(1.0))],
            ),
            file(
                "c.rs",
                Attribution::Resolved(Resolution::Unresolved),
                vec![function("c", "c.rs", 1, 1, None)],
            ),
            file(
                "d.rs",
                Attribution::Resolved(Resolution::Ambiguous(vec!["x/d.rs", "y/d.rs"])),
                vec![function("d", "d.rs", 1, 1, None)],
            ),
            file(
                "e.rs",
                Attribution::Collision {
                    key: "e.rs",
                    sources: vec!["e.rs".to_string(), "x/e.rs".to_string()],
                },
                vec![function("e", "e.rs", 1, 1, None)],
            ),
            file(
                "f.rs",
                Attribution::Superseded {
                    key: "f.rs",
                    winner: "x/f.rs".to_string(),
                },
                vec![function("f", "f.rs", 1, 1, None)],
            ),
        ];

        let document =
            render(&files, Vec::new(), CoverageSourceEcho::Provided, 0, &[]).expect("render");
        let caveats: BTreeMap<String, Value> = parsed(&document)["functions"]
            .as_array()
            .expect("functions is an array")
            .iter()
            .map(|f| (f["name"].to_string(), f["coverage_caveat"].clone()))
            .collect();

        assert_eq!(caveats["\"a\""], Value::Null);
        assert_eq!(caveats["\"b\""], json!("suffix"));
        assert_eq!(caveats["\"c\""], json!("absent"));
        assert_eq!(caveats["\"d\""], json!("ambiguous"));
        assert_eq!(caveats["\"e\""], json!("contested"));
        assert_eq!(caveats["\"f\""], json!("superseded"));
    }

    /// C24: the `code` strings are the contract consumers branch on. They are
    /// **append-only and never renamed** — a new `Kind` adds a string here, and
    /// no existing string in this list may ever change. Renaming one is a
    /// breaking change even though nothing in this crate would notice.
    #[test]
    fn every_diagnostic_code_is_byte_locked() {
        let codes: Vec<&str> = all_kinds()
            .iter()
            .map(|kind| Diagnostic::global(kind.clone()).code)
            .collect();
        assert_eq!(
            codes,
            [
                "ignored-test-command",
                "test-command-without-lcov-token",
                "module-file-missing",
                "module-file-ambiguous",
                "module-path-conditional",
                "coverage-suffix-match",
                "coverage-ambiguous",
                "coverage-absent",
                "coverage-contested",
                "coverage-superseded",
                "filter-matched-nothing",
            ]
        );
    }

    /// Every warning is renderable: `data` names every variant explicitly, so
    /// this also proves no `Kind` reaches the wire with a panic or a hole.
    #[test]
    fn every_kind_renders_a_warning_with_its_particulars() {
        for kind in all_kinds() {
            let diagnostic = Diagnostic::run(Site::at("src/lib.rs", 2, 5), kind.clone());
            let rendered = render(
                &[],
                Vec::new(),
                CoverageSourceEcho::Provided,
                0,
                &[&diagnostic],
            )
            .expect("render");
            let warning = parsed(&rendered)["warnings"][0].clone();

            assert_eq!(warning["severity"], json!("warning"));
            assert_eq!(warning["phase"], json!("run"));
            assert_eq!(warning["file"], json!("src/lib.rs"));
            assert_eq!(warning["line"], json!(2));
            assert_eq!(warning["column"], json!(5));
            assert_eq!(warning["message"], json!(kind.to_string()));
            assert!(warning["data"].is_object(), "{warning}");
        }
    }

    /// FC-T10c, on the wire: a `winner` naming a file that has no row is the
    /// documented, correct outcome of C26 — attribution is whole-workspace
    /// truth while `functions[]` is narrowed.
    #[test]
    fn a_cross_reference_may_name_a_file_with_no_row() {
        let diagnostic = Diagnostic::run(
            Site::file("crates/beta/src/lib.rs"),
            Kind::CoverageSuperseded {
                key: "src/lib.rs".to_string(),
                winner: "crates/alpha/src/lib.rs".to_string(),
            },
        );
        let files = [file(
            "crates/beta/src/lib.rs",
            Attribution::Superseded {
                key: "src/lib.rs",
                winner: "crates/alpha/src/lib.rs".to_string(),
            },
            vec![function("beta", "crates/beta/src/lib.rs", 1, 1, None)],
        )];

        let document = parsed(
            &render(
                &files,
                vec!["crates/beta"],
                CoverageSourceEcho::Provided,
                0,
                &[&diagnostic],
            )
            .expect("render"),
        );

        assert_eq!(
            document["warnings"][0]["data"]["winner"],
            json!("crates/alpha/src/lib.rs")
        );
        let rows: Vec<&str> = document["functions"]
            .as_array()
            .expect("functions is an array")
            .iter()
            .map(|f| f["file"].as_str().expect("file is a string"))
            .collect();
        assert_eq!(rows, ["crates/beta/src/lib.rs"]);
    }

    /// FC-T10e: `functions.length` is the row count, so no second count is
    /// emitted beside it — a redundant count is a second source of truth that
    /// can disagree with the first. `declined_modules` is present even when
    /// nothing was declined, so a consumer never has to tell "absent" from
    /// "zero".
    ///
    /// The key *order* is locked by [`the_document_shape_is_frozen`]; parsing
    /// sorts keys, so this asserts the key *set*.
    #[test]
    fn the_document_carries_declined_modules_and_no_row_count() {
        let document =
            parsed(&render(&[], Vec::new(), CoverageSourceEcho::Provided, 0, &[]).expect("render"));
        assert_eq!(top_level_keys(&document), TOP_LEVEL_KEYS);
        assert_eq!(document["declined_modules"], json!(0));
    }

    /// One of every kind, in `code()` order.
    fn all_kinds() -> Vec<Kind> {
        vec![
            Kind::IgnoredTestCommand,
            Kind::TestCommandWithoutLcovToken {
                token: "{lcov}",
                artifact: "target/crap4rust/coverage.lcov".to_string(),
            },
            Kind::ModuleFileMissing {
                module: "demo::absent".to_string(),
                candidates: vec!["src/absent.rs".to_string()],
            },
            Kind::ModuleFileAmbiguous {
                module: "demo::foo".to_string(),
                candidates: vec!["src/foo.rs".to_string(), "src/foo/mod.rs".to_string()],
                analysed: "src/foo.rs".to_string(),
            },
            Kind::ModulePathConditional {
                module: "demo::imp".to_string(),
                attribute: "#[cfg_attr(windows, path = \"windows.rs\")]".to_string(),
            },
            Kind::CoverageSuffixMatch {
                key: "src/lib.rs".to_string(),
            },
            Kind::CoverageAmbiguous {
                keys: vec!["a/src/lib.rs".to_string(), "b/src/lib.rs".to_string()],
            },
            Kind::CoverageAbsent,
            Kind::CoverageContested {
                key: "src/lib.rs".to_string(),
                claimants: vec!["a/src/lib.rs".to_string(), "b/src/lib.rs".to_string()],
            },
            Kind::CoverageSuperseded {
                key: "src/lib.rs".to_string(),
                winner: "a/src/lib.rs".to_string(),
            },
            Kind::FilterMatchedNothing {
                filter: "crates/gone".to_string(),
            },
        ]
    }
}
