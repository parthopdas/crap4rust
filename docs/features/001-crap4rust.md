<!-- Save as docs/features/<nnn>-<feature_name>.md — <nnn> = zero-padded next sequence number (highest existing + 1). -->
# Feature: crap4rust — CRAP-metric quality gate for Rust
**Branch:** vibe/001-crap4rust
**Status:** In progress — S1 (walking skeleton) complete; next slice S2

## Requirements

A CLI tool (`crap4rust`) that computes the **CRAP** metric (Change Risk Anti-Patterns, classic/Uncle-Bob:
`CRAP = cc² × (1 − cov)³ + cc`, where `cov` = fraction line coverage) per function for Rust cargo
workspaces, and gates CI/agents on it. A Rust port of Uncle Bob's `unclebob/crap4go`.

- **Complexity**: own cyclomatic complexity (CC) via `syn`, source-level (no macro expansion).
- **Coverage**: line coverage from LCOV `DA` records; zero-config runs `cargo-llvm-cov`.
- **Surfaces**: (1) human table with crap4go parity — header "CRAP Report", 5 columns, risk bands
  1–5 / 5–30 / 30+; (2) versioned JSON contract.
- **Exit codes (C6 — crap4go parity)**: `0` on success (reports even high-CRAP functions), `1` on
  operational error. **No CRAP-threshold gate in v1** — the tool reports; it does not fail the build.
  The gate + baseline/delta are deferred (see Deferrals).
- **Scope**: cargo workspace / monorepo.
- Guiding principle (human, literal): _"keep parity with crap4go in terms of user experience, general
  core arithmetic; keep rust native and idiomatic others."_

### CC counting rules (C1 — locked)

Base +1, then +1 each for: `if`/`else if`, `while`/`while let`, `for`, `loop`, each `match` arm
**whose pattern is not the wildcard `_`** (decision B, human 2026-08-24: **only `_` is exempt** — a bare
binding like `other`/`x`, a unit variant like `None`, and every non-`_` pattern all count +1), `&&`,
`||`, `if let`/`let-else`, **`?` (try operator)**, and **match-arm guards counted separately** (a guard
`if` in an arm always adds +1, independent of the pattern — so a guarded wildcard `_ if cond` = +1 for
the guard only, decision A). Decisions inside closures are attributed to the enclosing fn. Source-level
only; async/macro-generated control flow is not expanded (documented limit).

## Design Options (Ox)

### O1 — Wrap external tooling (complexity crates + `cargo-llvm-cov`)
- Description: parse an external CC analyzer (e.g. `rust-code-analysis`) plus `cargo-llvm-cov` output.
- Pros: least code; fast to stand up.
- Cons: CC semantics not ours → can't guarantee crap4go arithmetic parity or the locked C1 rules;
  brittle on tool output drift; weak control over naming/filtering; depends on a semi-stale analyzer.

### O2 — Own CC via `syn`; coverage via `cargo-llvm-cov` (**chosen**)
- Description: parse crate AST with `syn` and own CC + function identity; consume LCOV line coverage
  from `cargo-llvm-cov`; join on source spans.
- Pros: full control of CC semantics (C1 parity + Rust extensions like `?`); clean layering
  (CC / coverage-adapter / CRAP-domain / reporters / CLI); testable core with no I/O;
  `cargo-llvm-cov` is the de-facto Rust coverage standard and emits LCOV; single healthy external dep.
- Cons: we own CC edge cases (async, macros, generics naming); must join `syn` spans to LCOV line hits.

### O3 — Full custom coverage (MIR/instrumentation) + own CC
- Description: reimplement coverage acquisition from compiler artifacts.
- Pros: maximal fidelity, region/branch coverage possible.
- Cons: massive scope, toolchain-version-fragile, reinvents `llvm-cov`. YAGNI.

**Recommended: O2 — parity where it matters (arithmetic, UX), idiomatic Rust elsewhere; layered and
unit-testable, with LCOV as a stable coverage seam.**

## Slices (Sx)

A slice is defined in `docs/meta-design.md`. Each slice is independently end-to-end verifiable.
Walking skeleton first, then layer outward.

| Slice | Outcome | Depends on |
|-------|---------|------------|
| S1 | **Walking skeleton**: `crap4rust --lcov-path X <crate>` parses one crate's `syn` AST, computes CC + CRAP from provided LCOV, prints "CRAP Report" table. No coverage running, no gate. | – |
| S2 | **Zero-config coverage**: run `cargo llvm-cov` → `target/crap4rust/coverage.lcov`, then compute. `--test-command` override, `--lcov-path` BYO. | S1 |
| S3 | **Workspace/monorepo**: enumerate workspace members; Package→Module crate-qualified naming; product-vs-test filtering; positional path-fragment filters. | S1 |
| S4 | **JSON contract**: versioned `--format json` output. | S1, S3 |
| S5 | **Polish/perf**: `rayon` parallelism (`-j/--jobs`), display-band coloring, docs of async/macro limits, error UX. | S2–S4 |

## Tasks (Tx)

One or more tasks per slice.

| #   | Slice | Task | Status  | Commit |
|-----|-------|------|---------|--------|
| T1  | S1 | **CC engine (`syn` visitor)** — walk items/exprs, apply C1 rules; attribute closure decisions to enclosing fn. _Assumes source-level only._ | **Done** | vibe/001 |
| T2  | S1 | **Fn identity/naming** — `<Type as Trait>::method` form; free fns, impl methods, closures folded into parent (C4). | **Done** | vibe/001 |
| T3  | S1 | **LCOV reader** — parse `DA` records → per-file line-hit map. | **Done** | vibe/001 |
| T4  | S1 | **CRAP domain** — `crap(cc, cov)`; band classifier (1–5 / 5–30 / 30+), independent of `--threshold` (C11). Pure, no I/O. | **Done** | vibe/001 |
| T5  | S1 | **Coverage join** — intersect fn `syn` span lines with LCOV covered/total → per-fn `cov`. | **Done** | vibe/001 |
| T6  | S1 | **Table reporter** — header "CRAP Report", 5 columns, crap4go layout parity. | **Done** | vibe/001 |
| T7  | S1 | **CLI skeleton** — arg parse, wire pipeline, `--lcov-path`. Also lands C15 (identity fields). | **Done** | vibe/001 |
| T8  | S2 | **Coverage runner** — invoke `cargo llvm-cov` → default `target/crap4rust/coverage.lcov`; `--test-command` override; zero-config default (C2). Makes `--lcov-path` optional — `missing_required_lcov_arg_exits_one` legitimately changes (expected evolution, not a regression). | Pending | - |
| T9  | S3 | **Workspace enumeration** — cargo metadata; per-member modules, crate-qualified (C3). Checklist: replace `report::rows_from_joined` module derivation (FC-T6a); move source discovery out of `cli.rs` into its adapter; FC-T5a longest-overlap + `Ambiguous` resolution; FC-T5e `SourceUnit` refactor. | Pending | - |
| T10 | S3 | **Product/test filtering** — exclude `#[cfg(test)]`/`#[test]`; `tests/`/`benches/`/`examples/` non-product; positional path-fragment filters (C5). | Pending | - |
| T11 | S4 | **JSON reporter** — versioned schema (`schema_version`), stable field contract. | Pending | - |
| T12 | S5 | **Parallelism** — `rayon` across members/files; `-j/--jobs`, full CPUs default (C7). | Pending | - |
| T13 | S5 | **Docs & limits** — async/macro CC caveats (C12), README, `--help`. | Pending | - |

### Test expectations (inline)

- **T1 (CC) unit tests**: table-driven per construct — empty fn = 1; `if`/`else if` chain; `match`
  with & without catch-all wildcard; `&&`/`||`; `if let`/`let-else`; `?`; match-arm **guard** counted
  separately; nested closure decisions attributed to parent; `while let`/`loop`/`for`.
- **T4 (CRAP arithmetic) unit tests**: known vectors, e.g. `cc=1,cov=1→1`; `cc=5,cov=0→5²+5=30`;
  `cc=5,cov=1→5`; band boundaries at 5 and 30.
- **T5 coverage-join integration**: fixture crate + fixture LCOV → expected per-fn `cov`;
  partial/zero/full coverage.
- **T7 CLI exit-code integration (crap4go parity)**: normal run (even with high-CRAP functions) → 0;
  bad args / coverage-command failure / unparseable LCOV → 1. No threshold gate in v1.

## Risks (Rx)

- **R1** — `syn` span → LCOV line mapping drift (attributes/macros shift lines); mitigate with
  fixture-based join tests (T5).
- **R2** — CC naming for generics/trait impls may mismatch coverage attribution; pin via T2 tests.
- **R3** — `cargo-llvm-cov` toolchain/version variance in CI; mitigate with `--lcov-path` BYO seam.
- **R4** — Macro/async under-counting could understate CRAP; explicitly documented limit (C12/T16),
  not silently wrong.
- **R5** — Deferring the gate means v1 has no enforcement surface; acceptable — v1 is the faithful
  port, the guardrail gate lands in a later feature (D1).

## Assumptions (Ax)

- **A1** — `cargo-llvm-cov` available on PATH for zero-config; otherwise user supplies `--lcov-path`.
- **A2** — Line coverage (LCOV `DA`) is the coverage semantic; no branch/region coverage.
- **A3** — Repo is a valid cargo workspace resolvable via cargo metadata.
- **A4** — Source-level `syn` parse suffices; no `cargo expand`/macro expansion.

## Deferrals (Dx)

- **D1** — **CRAP-threshold gate + baseline/delta mode** (exit `2` on breach, `--threshold`, git-hunk
  in-scope detection vs `git diff <baseline>...HEAD`, gate only new/changed fns): **deferred at human
  request (2026-08-24)** — v1 keeps crap4go parity (report-only). This is the future guardrail hook for
  agentic gating.
- **D2** — **Dogfooding into this repo's `crap-check` gate: NO** (explicit deferral).
- **D3** — Branch/region coverage; per-line region CRAP.
- **D4** — Macro-expanded CC (`cargo expand` integration).
- **D5** — Non-cargo projects.
- **D6** — SARIF / other report formats beyond table + JSON.
- **D7** — Incremental/cached CC across runs.

## Notes & Decisions
### C16 — Bidirectional path matching — **DELIBERATE DIVERGENCE (verified, 2026-08-24)**

crap4go's `suffixMatch` (`internal/coverage/coverage.go`, `unclebob/crap4go` @ `bee16db`) is
**one-directional** — it returns `false` as soon as `len(suffixParts) > len(pathParts)`, so only the
queried CC file path may be a suffix of the LCOV/profile key, never the reverse. crap4rust matches in
**either** direction, because LCOV `SF:` keys are short/relative while the CC engine emits absolute
paths; the one-directional rule would resolve nothing and print `N/A` for every function. Verified
against upstream source. **FC-T5f reconciled** (T7).

Secondary, recorded: our resolution order is **deterministic** (`BTreeMap`, lowest key wins) where
upstream's Go map iteration is randomized. `normalizePath` parity is now **verified**, not assumed
(`\`→`/`, strip one leading `./`). `CoverageForRange` returning `0.0` at `total==0` re-confirmed, so
the C13 divergence stands as written.

### T7 review notes (Anders — approve-with-suggestions, 2026-08-24)

T7 (`src/cli.rs` + `src/main.rs`; `clap` derive + `anyhow`) closes **S1**. Bhaskar FAIL → fix → PASS
(66 tests: 56 lib + 10 CLI integration; fmt/clippy/build/test clean), real-binary exit codes and
stream isolation verified. Anders: "the skeleton walks — end-to-end and honest at every seam."

Landed: pipeline wired (sorted `.rs` walk → CC → LCOV → join → rows → table → stdout via `print!`);
exit codes 0/1 only via `try_parse` (never clap's default 2); errors/diagnostics → stderr;
**C15** identity fields added additively with the C14 bytes unchanged; **FC-T5b** satisfied via
`Resolution{Exact,Suffix,Unresolved}` + `JoinResult{functions,resolutions}` — status **as data**, the
CLI edge does the printing, one diagnostic per source *file*, exit code unaffected; **FC-T4a** audit
done (`LcovData::file()` made `#[cfg(test)]`-only rather than `#[allow]`-masked).

Discharged: FC-T4a, FC-T5b, FC-T5d/C15, FC-T5f, FC-T6c, FC-T6e.
Still parked: FC-T5a, FC-T5e (T9), FC-T5c (T13 docs), FC-T6a/b/d/f.

New forward constraints:
- **FC-T7a (T11 — identity portability).** C15's `file` is the **raw** path as given (`src\lib.rs` on
  Windows, absolute if the user passed absolute). The table is insulated (`module_from_path`
  normalizes); a JSON `file` field is not. **T11 must emit a normalized, forward-slashed,
  workspace-relative `file`** or `schema_version` freezes a platform-dependent identity key.
- **FC-T6b extension.** Determinism now covers `resolutions`/diagnostics too — diagnostic order
  follows input order; T12 must preserve it.
- **FC-T7b (`RunOutput` invariant).** One field per output stream, nothing else. No exit codes, no
  summary counts, no "exceeded the band" flags — a third non-stream field means the deferred gate
  (D1) is leaking in early. T11 selects the reporter **inside** `run`; format branching must not move
  into `main.rs`.
- **FC-T7c (T8 — `RunConfig`).** Once T8/T9/T12 add `--test-command`, workspace roots and `-j`,
  introduce a plain `RunConfig` with `From<&Cli>` so `run` is exercisable without an arg parser.
  Not worth it at two fields.
- **FC-T7d (known temporary deviation).** `docs/design.md` draws workspace/source discovery as an
  **adapter**; for S1 it lives in `cli.rs`. Owed to T9 — not the settled architecture.
- **FC-T7e (T11 product question).** Diagnostics are pre-rendered `Vec<String>`, stderr-only. Fine
  forever **if** JSON output also emits them only to stderr. If resolution warnings should appear
  inside the JSON document, `Vec<String>` is the wrong carrier and the structured form must exist
  **before** `schema_version` freezes. Rule at T11.

Non-blocking, unactioned (recorded so they are not rediscovered): the S1 walk descends into `target/`
and dot-directories — cheap two-line guard, or document "point it at `src/`" and defer wholly to T10;
no symlink-cycle guard (T9's cargo-metadata enumeration removes the exposure);
`a_directory_is_walked_for_rs_files_in_sorted_order` leans on cargo's cwd — `CARGO_MANIFEST_DIR`
would make it stand alone.

### C15 — Function identity (FC-T5d) — **RESOLVED (human, 2026-08-24)**

**Decision: Anders' option 1.** Additively carry `file` + `start_line` through
`FunctionComplexity` → `JoinedFunction` → `ReportRow`, **at T7** (during the `allow(dead_code)`
audit). The **v1 table is unchanged** — C14 layout and its byte-locked tests stay exactly as-is;
duplicate-looking rows are accepted in the table and documented as a limit in T13. The fields exist so
T11's JSON contract has a stable identity key before `schema_version` is frozen. Also resolves
**FC-T6c** (`ReportRow` no longer drops `file`).

### T6 review notes (Anders — approve-with-suggestions, 2026-08-24)

T6 (`src/report.rs`: `ReportRow{name,module,complexity,coverage,crap}`, pure
`format_report(&[ReportRow])->String`, adapter `rows_from_joined`, placeholder `module_from_path`)
passed Bhaskar's full gate (45 tests, fmt/clippy/build/test clean) and Anders' design review. C14
layout is byte-locked by 9 tests (Go `%-30s %-35s %4s %7s %8s`; 88-char header/separator; overflow, no
truncation; `   N/A ` / `     N/A`; CRAP desc, `None` last, name-asc tie-break via `total_cmp`).

Assumptions recorded (Dave): module is an **opaque string supplied to the reporter** — the reporter
never derives crate identity (S3/T9 swaps the adapter, not the formatter); `module_from_path` is an
explicit S1 placeholder (path normalized, `\`→`/`, `./` stripped); trailing in-cell whitespace kept
for parity; empty input still emits the 4 preamble lines; no band column (FC-T6 honored).

Forward constraints:
- **FC-T6a (schema freeze ordering).** `schema_version` must NOT be frozen while `module` is the path
  placeholder — **T9 is a hard predecessor of T11.** Add "replace `report::rows_from_joined` module
  derivation" to T9's checklist.
- **FC-T6b (parallel determinism).** `compare` is total except when CRAP **and** name both tie
  (the duplicate-display-name case). T12 must preserve input ordering — order-preserving rayon
  `map`/`collect`, never `par_bridge` or iteration over a `HashMap`; sort source units first.
- **FC-T6c (JSON row seam).** `ReportRow` drops `file`. Before T11, either carry `file` on `ReportRow`
  or rule that the JSON row derives from `JoinedFunction` — leaving it undecided forks the seam.
- **FC-T6d (shared ordering).** T11 must reuse C14 ordering, not copy `compare`; hoist to a shared
  helper or have T7 sort once and hand both reporters a pre-ordered slice.
- **FC-T6e (T7 printing).** `format_report` output is fully newline-terminated — T7 uses `print!`,
  not `println!`.
- **FC-T6f (band coloring, S5).** Bands arrive as ANSI color on existing cells only — never an extra
  column or width change, or the C14-locked tests break.

**Open for the human — same-display-name disambiguation (FC-T5d).** Duplicate-looking rows are
emitted today. Anders' recommendation (human decides): additively carry `file` + `start_line` through
`JoinedFunction`→`ReportRow` **at T7** (during the `allow(dead_code)` audit), keep the v1 table
unchanged, document as a T13 limit — hard deadline **before T11**, since a JSON array with no stable
identity key would freeze a contract defect into `schema_version`.

### T4 review notes (Anders — approve-with-suggestions, 2026-08-24)

T4 (`src/crap.rs`: pure `crap(cc:u32,cov:f64)->f64`, `score(cc,Option<f64>)->Option<f64>`,
`band(f64)->RiskBand{Low,Moderate,High}`) passed Bhaskar full gate (28 tests) and Anders' review.
Boundary inclusivity: `≤5→Low`, `5<..≤30→Moderate`, `>30→High` (so 5.0→Low, 30.0→Moderate) — internal
presentation, C11-only (crap4go has no bands), no parity contract, documented at the fn (no divergence
entry needed). `Option<f64>` is the domain↔join seam; C13 `total=0⇒cov=1` is NOT encoded here (T5 owns it).

Forward constraints to honor:
- **FC-T4a (dead_code plan correction).** `crap`/`score` go live at T5/T6; **`band`/`RiskBand` stay
  unused until S5 (display-band coloring)**. Do NOT expect a clean blanket removal of
  `#![allow(dead_code)]` from `crap.rs` at T7 — keep a **targeted** `#[allow(dead_code)]` on
  `band`/`RiskBand` (or defer the module-allow removal) until S5. The module doc-comment's "remove at
  T5/T7" line is inaccurate for `band`; fix when reaching T5.
- **FC-T4b (domain invariant).** `crap()` assumes `cov ∈ [0.0,1.0]`. **T5 MUST deliver
  `cov ∈ [0.0,1.0]` or `None`** (no NaN, no out-of-range) — else `(1−cov)³` goes negative / NaN
  propagates and `band(NaN)` silently returns High. T5 owns this enforcement (optionally a
  `debug_assert!` could be added to `crap()` later).
- **FC-T6 (no band column).** T6's table is C14's 5 columns only (Function·Module·CC·Cov%·CRAP);
  `band()` is NOT a T6 dependency — do not wire it into the reporter table.

### T5 review notes (Anders — approve-with-suggestions, 2026-08-24)

T5 (pure `join` module + `resolve_path` path resolution on `LcovData`) passed Bhaskar (full gate, 36
tests) and Anders' design review. C13 contract, layering, and forward-fit all confirmed. Forward
constraints:

- **FC-T5a (T9 — collision guard).** Bidirectional suffix match returns the lexicographically-first
  `BTreeMap` key among candidates — silent/arbitrary under cross-member filename collisions
  (`src/lib.rs`, `mod.rs`). Before workspace support, replace first-match with **longest-overlap
  preference**; treat a genuine tie as **ambiguous ⇒ diagnostic + unresolved (N/A)**, not a silent pick.
  **S1 is immune (single crate); this is a T9 gate.** Bidirectional itself is correct (LCOV keys are
  short/relative, CC paths may be absolute — one-directional would miss and yield N/A everywhere); keep
  it.
- **FC-T5b (T7 — observable resolution).** When a file resolves via **non-exact** suffix match, or fails
  to resolve, emit a stderr diagnostic (no effect on exit code — C6). Discharges the original
  "silent + dangerous" objection once the CLI provides the channel.
- **FC-T5c (T6 — Cov% of `total==0`).** A `total==0 ⇒ Some(1.0)` fn renders `100.0%`, indistinguishable
  from genuine full coverage. Intended per C13(b); T6 just confirms no desire to mark it distinctly. No
  join change.
- **FC-T5d (T6 — line hedge).** If same-display-name disambiguation lands on `file:line`,
  `JoinedFunction` must carry the source line; consider adding `start_line` now to avoid rework.
- **FC-T5e (T9 — input shape).** `&[(String, Vec<FunctionComplexity>)]` carries no package identity; C3
  crate-qualified Module needs it. Thread package context additively (or promote the tuple to a named
  `SourceUnit`) without disturbing `resolve_path`.
- **FC-T5f (docs — parity claim).** Reconcile the `is_suffix`/`resolve_path` "mirrors crap4go" comment
  vs the note that crap4go is one-directional: verify crap4go's `segmentsForFile`/`suffixMatch`, and if
  it differs, keep the bidirectional **behavior** but record it as a deliberate divergence (like C13) —
  don't leave a false parity claim in the source. Reconcile before T9/merge.

Consistency confirmed: `join.rs`'s `#![allow(dead_code)]` "remove at T7" is correct (whole surface
test-only, no band-like S5 holdout — differs from `crap.rs`'s FC-T4a targeted keep). C13, FC-T3
(`try_from…unwrap_or(u32::MAX)`), FC-T4b invariant (`total==0` short-circuits before division ⇒ no
NaN/out-of-range) all faithfully encoded. No code changes requested.

### T3 review notes (Anders — approve-with-suggestions, 2026-08-24)

T3 (LCOV reader: pure `parse_lcov` + thin `load`, `coverage_in_range(file,start,end)->(covered,total)`,
typed `LcovError`, `thiserror` dep) passed Bhaskar (full gate, 23 tests) and Anders' review. Forward
constraints for T4/T5:

- **T5 path normalization (high risk).** LCOV `SF:` paths (workspace-relative or absolute, incl. Windows
  `C:\...`) will NOT match the CC engine's file paths byte-for-byte. A mismatch does not error — it
  silently returns `(0,0)` ⇒ `cov=0` ⇒ **inflated CRAP on every function**. T5 must normalize both sides
  to a common key AND use the `file()` `Option` accessor to detect a total LCOV-file miss (a bug signal)
  distinctly from "function has no instrumented lines."
- **T5 span type seam** — `FunctionComplexity` spans are `usize`; `coverage_in_range` takes `u32`. Use a
  conscious `usize→u32` cast/`try_into` at the boundary.
- **OPEN product decision (see C13 below) — `cov` when `total=0`** (function has no instrumented lines):
  undefined `covered/total`. Swings CRAP maximally (`cov=0` ⇒ worst band; `cov=1` ⇒ best). Must match
  crap4go. **Pending human ruling; needed before T5.**

### T2 review notes (Anders — approve-with-suggestions, 2026-08-24)

T2 (function identity/naming via a context stack on `FnCollector`) passed Bhaskar (full gate, 12 tests)
and Anders' review. Forward constraints recorded for later tasks:

- **Join keys on `(file, span)`, NOT on `name`.** Function names are **presentation-only** and can
  collide (e.g. `impl Foo<u8>`/`impl Foo<u16>` both render `Foo::bar`; alias vs fully-qualified trait
  paths). T5's coverage join and any aggregation/dedup MUST key on `(file, span)` so distinct functions
  never collapse. (Pinned as a T5 constraint.)
- **T9 crate/file-module qualification** — the current `name` is **file-local**: it reflects only
  inline `mod {}` blocks, not file-based modules (`src/foo/bar.rs` ⇒ `foo::bar`). T9 must *prepend* the
  crate + file-path module prefix on top of this; it must not assume the name is already crate-relative.
- **`node.span()` includes attributes/doc-comments** so `start_line` may sit above the body — harmless
  for the join (attribute lines carry no LCOV `DA` records). Do not narrow the span "fix" this later.
- **Open product decision (T6/T9 reporter/UX):** how to disambiguate genuinely-distinct functions that
  render to the same display name — include generic args, append `file:line`, or accept duplicate-named
  rows. Deferred to the reporter tasks; flagged for the human.

### T1 review notes (Anders — approve-with-suggestions, 2026-08-24)

T1 (CC engine + minimal cargo scaffold) passed Bhaskar (full gate green) and Anders' design review.
Structural decisions and forward constraints recorded for later tasks:

- **lib + bin layout** (endorsed): pure core lives in the library crate; `src/main.rs` is a thin
  entrypoint. Required anyway by `test:quick = cargo test --lib --bins`.
- **(a) T7 CLI seam** — expose a single thin `pub fn run(...) -> ExitCode` in the lib; `main.rs` is a
  one-line shim. **Keep the CC engine `pub(crate)` — do NOT widen it to `pub`** across the crate
  boundary.
- **(b) `#![allow(dead_code)]`** in `complexity.rs` is acceptable scaffolding (whole surface is
  test-only until the pipeline is wired). **Remove it as a checklist item on T5/T7**, not "someday."
- **(c) T2** — `FnCollector` must add enclosing-`impl`/`mod` context tracking (push/pop) to build
  `<Type as Trait>::method` and crate-qualified names.
- **(d)/(e) Or-pattern arms** — a match arm with an or-pattern (`A | B | C => ..`) counts **+1 for the
  arm, NOT +1 per alternative** (classic CC edge model; consistent with the locked per-arm refutability
  rule). **Confirmed by the human (2026-08-24).** Current code already does this; T2 adds a locking test
  plus an optional `async fn` (`.await` adds 0) test to document the C12 limit as behavior.

### C6 — Exit codes (crap4go parity)

**Decision (human, 2026-08-24)**: v1 mirrors crap4go exactly — **no CRAP-threshold gate**. crap4go
(`cmd/crap4go/main.go`) calls `os.Exit(1)` **only** on an operational error; otherwise it prints the
report and exits `0`, even for high-CRAP functions. crap4rust does the same:

- **0** — success; report printed (even when functions exceed the High band).
- **1** — operational error: bad args, coverage-command failure, unparseable LCOV.

The tool is a **reporter**, not a gate, in v1. The CI/agent gate (exit `2` on breach) and baseline/delta
scoping are **deferred** (see D1) — that's the future guardrail hook. Risk bands (1–5 / 5–30 / 30+)
still visually flag High-risk functions. With this change, **v1 has no intentional divergence from
crap4go behavior** — divergences are now only the language-mechanical ones plus the additive JSON output.

### Locked rulings (recorded for the loop)

- **C1** CC: `?` counts; match-arm guards count separately; base +1 plus the faithful rule set;
  closures → enclosing fn; source-level, no macro expansion.
  - **C1 addendum — match-arm counting (refutability model).** A match arm's pattern counts **+1**
    unless it is the wildcard `_` (`Pat::Wild`), which counts **+0**; an arm **guard** (`if` in the arm)
    always counts **+1** additionally. **A (settled):** guarded wildcard `_ if cond` = +1 (guard only;
    `_` never counts). **B (human 2026-08-24, option i):** only `_` is exempt — every bare `Pat::Ident`
    (binding `other`, unit variant `None`) and all other pattern kinds count +1. Chosen for simplicity
    and the safe (never-undercount) direction; may overcount a genuine binding catch-all `other =>` by
    1 — an accepted, documented source-level limitation (C12).
- **C2** zero-config runs `cargo llvm-cov`; default artifact `target/crap4rust/coverage.lcov`;
  `--lcov-path` BYO; `--test-command` override.
- **C3** Package → Module, crate-qualified naming.
- **C4** `<Type as Trait>::method` naming; closures attributed to enclosing fn.
- **C5** exclude `#[cfg(test)]`/`#[test]`; `tests/`/`benches/`/`examples/` non-product by default;
  positional path-fragment filters.
- **C7** `rayon`, full CPUs, Cargo-style `-j/--jobs`.
- **C10** baseline/delta gating: **deferred** (see D1); v1 reports all functions, no gating.
- **C11** fixed display bands (1–5 / 5–30 / 30+); report-only (no `--threshold` gate in v1).
- **C12** source-level only; async/macro limits documented.

### C13 — `cov` semantics: `total=0` and file-absent — **RESOLVED (human, 2026-08-24)**

**Decision: option (b) — `total=0 ⇒ cov=1`** (function has no instrumented lines ⇒ nothing to test ⇒
no risk ⇒ best band). This is a **deliberate divergence from crap4go**, which returns `0.0` (0%) for
`total==0`. Rationale: a zero-instrumented-line function is not under-tested, so it should not be flagged.

File-absent handling is **kept at parity**: `file()==None` ⇒ `cov = None` ⇒ reported `N/A`, unscored,
sorts last.

Join contract for T5 (given `coverage_in_range → (covered,total)` and `file()`):
- `file()==None` ⇒ `cov=None` (N/A, unscored).
- `file()==Some` & `total==0` ⇒ `cov=Some(1.0)`.
- `file()==Some` & `total>0` ⇒ `cov=Some(covered/total)`.

Source-verified crap4go behavior (for the record; we diverge only on `total=0`):
- File absent ⇒ `CoverageForRange` returns `nil` ⇒ `Score` returns `nil` ⇒ `N/A`, sorts last.
- File present, `total==0` ⇒ returns `0.0` ⇒ cov 0% (**we instead use `cov=1`**).

### C14 — Table reporter format (T6, crap4go parity)

Source-verified from `internal/crap/crap.go` `FormatReport`. T6 mirrors it:

- Lines: `"CRAP Report"`, then `"==========="`, then header, then a `-`-repeat separator sized to the header.
- Header/rows: `Function` (left, 30) · `Package`→**Module** (left, 35) · `CC` (right, 4) · `Cov%`
  (right, 7) · `CRAP` (right, 8). (Go fmt `%-30s %-35s %4s %7s %8s`.)
- Cell formats: Cov% = `%5.1f%%` (e.g. ` 87.5%`) or `  N/A ` when `None`; CRAP = `%8.1f` or `     N/A`.
- **Sort by CRAP descending**; `None` CRAP sorts last; ties broken by `Name` ascending (stable).

### Architecture (O2 — dependency flow, Clean Architecture)

Layering (dependencies point inward): **`syn` CC engine (pure)** and **CRAP domain (pure)** depend on
nothing → **coverage (LCOV) adapter**, **reporters (table/JSON)**, and **CLI** depend inward. Keep I/O
at the edges so CC and CRAP stay unit-testable without on-disk fixtures.

### Driver action — seed `docs/design.md`

`docs/design.md` is still a `FILL_ME` stub and the loop is **preflight-gated** on it. As part of **S1**,
the driver seeds `docs/design.md` from this feature's O2 architecture (layering above) so preflight
passes and the loop can start. (Anders does not write `docs/design.md`.)
