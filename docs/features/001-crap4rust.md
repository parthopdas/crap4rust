<!-- Save as docs/features/<nnn>-<feature_name>.md — <nnn> = zero-padded next sequence number (highest existing + 1). -->
# Feature: crap4rust — CRAP-metric quality gate for Rust
**Branch:** vibe/001-crap4rust
**Status:** Planning

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
| T4  | S1 | **CRAP domain** — `crap(cc, cov)`; band classifier (1–5 / 5–30 / 30+), independent of `--threshold` (C11). Pure, no I/O. | Pending | - |
| T5  | S1 | **Coverage join** — intersect fn `syn` span lines with LCOV covered/total → per-fn `cov`. | Pending | - |
| T6  | S1 | **Table reporter** — header "CRAP Report", 5 columns, crap4go layout parity. | Pending | - |
| T7  | S1 | **CLI skeleton** — arg parse, wire pipeline, `--lcov-path`. | Pending | - |
| T8  | S2 | **Coverage runner** — invoke `cargo llvm-cov` → default `target/crap4rust/coverage.lcov`; `--test-command` override; zero-config default (C2). | Pending | - |
| T9  | S3 | **Workspace enumeration** — cargo metadata; per-member modules, crate-qualified (C3). | Pending | - |
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

### C13 — `cov` semantics: `total=0` and file-absent (crap4go parity) — **OPEN, needs human ruling**

Source-verified crap4go behavior (`internal/coverage/coverage.go` `CoverageForRange`, `internal/crap/crap.go`):

- **File absent from the coverage profile** ⇒ `CoverageForRange` returns `nil` ⇒ `crap.Score` returns
  `nil` ⇒ the function is **reported as `N/A`** for both Cov% and CRAP, and **sorts last** (unscored).
  Maps to crap4rust `file()` == `None`.
- **File present but the function's line range has no instrumented statements (`total==0`)** ⇒
  `CoverageForRange` returns `0.0` ⇒ **cov = 0%** ⇒ `CRAP = cc²·1 + cc` (max risk for that CC).
  Maps to crap4rust `file()==Some` with `total==0`.

**Parity ⇒ `total=0` means `cov=0`**, i.e. the OPPOSITE of Anders' earlier lean (`cov=1`). Because most
zero-statement functions are trivial (low CC), the CRAP inflation is bounded but non-zero. **Pending
human decision (before T5): (a) crap4go parity — `total=0 ⇒ cov=0`; (b) Anders' lean — `total=0 ⇒
cov=1`.** File-absent ⇒ `N/A`/unscored is adopted from parity regardless.

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
