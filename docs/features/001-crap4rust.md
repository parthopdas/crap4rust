<!-- Save as docs/features/<nnn>-<feature_name>.md — <nnn> = zero-padded next sequence number (highest existing + 1). -->
# Feature: crap4rust — CRAP-metric quality gate for Rust
**Branch:** vibe/001-crap4rust
**Status:** In progress — S1 + S2 complete; S3 in progress (T9 done, T10 next)

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
| S3 | **Workspace/monorepo**: enumerate workspace members; Package→Module crate-qualified naming; product-vs-test filtering; positional path-fragment filters. **Complete** (T9, T9b, T10). | S1 |
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
| T8  | S2 | **Coverage runner** — invoke `cargo llvm-cov` → default `target/crap4rust/coverage.lcov`; `--test-command` override; zero-config default (C2). Makes `--lcov-path` optional — `missing_required_lcov_arg_exits_one` legitimately changes (expected evolution, not a regression). | **Done** | vibe/001 |
| T9  | S3 | **Workspace enumeration** — cargo metadata; per-member modules, crate-qualified (C3). Checklist: replace `report::rows_from_joined` module derivation (FC-T6a); move source discovery out of `cli.rs` into its adapter; FC-T5a longest-overlap + `Ambiguous` resolution; FC-T5e `SourceUnit` refactor. | **Done** | vibe/001 |
| T9b | S3 | **Diagnostic unification + single-parse streaming** (human-inserted, 2026-08-24). Discharges FC-T9d (one `Diagnostic{code,severity,phase,site,kind}`, one sink drained on `Err` **and** `Ok`), FC-T9h (discovery streams each parsed AST; one parse per file) and FC-T9k (`workspace/` split). Also lands C18 (incomplete-report notice), C19 (exact-wins collisions), C20 (module/name normalisation). | **Done** | vibe/001 |
| T10 | S3 | **Product/test filtering** — exclude `#[cfg(test)]`/`#[test]`; `tests/`/`benches/`/`examples/` non-product; positional path-fragment filters (C5). Also FC-T9g (drop empty units before the join), FC-T9e (query key ≠ display path), FC-T9j (PATH is a locator; filters get their own surface). Also landed C22 (declined counts distinct modules), C25 (source-only MSRV floor + `msrv` CI job), C26 (filter is a reporting narrowing), C27 (zero-row report says so), C29 (case-sensitive). **Closes S3.** | **Done** | vibe/001 |
| T11 | S4 | **JSON reporter** — versioned schema (`schema_version`), stable field contract. | Pending | - |
| T12 | S5 | **Parallelism** — `rayon` across members/files; `-j/--jobs`, full CPUs default (C7). | Pending | - |
| T13 | S5 | **Docs & limits** — async/macro CC caveats (C12), README, `--help`. Must document: FC-T8f "no spaces in `--test-command`" (workaround `--lcov-path`); the C17 crap4go `--test-command` divergences + `{lcov}` vs `{coverprofile}` migration note; FC-T5c (`total==0` renders `100.0%`); C15 duplicate-display-name rows. | Pending | - |

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
### C17 — `--test-command` contract & crap4go divergences — **RESOLVED (human, 2026-08-24)**

Source-verified against `unclebob/crap4go` @ `bee16db` (`internal/cli/cli.go`, `cmd/crap4go/main.go`,
`internal/coverage/coverage.go`). crap4go **does** have `--test-command`, but it is **not** what an
earlier documentation-only reading suggested — three deliberate divergences, all human-ruled:

1. **argv, not a shell.** crap4go's `coverageCommand` returns `exec.Command("sh", "-c", command)` —
   the whole string goes to a POSIX shell (quoting/pipes/redirection work there) and it is
   **Unix-only**. crap4rust whitespace-splits into argv with **no shell**: our CI gates on
   `windows-latest`, where `sh -c` does not exist. Cost: a program or argument **containing spaces is
   inexpressible** (see FC-T8f). `strings.Fields` appears nowhere in crap4go's command path.
2. **`{lcov}` expands to a bare path**, where crap4go's `{coverprofile}` expands to the **whole flag**
   (`-coverprofile=…`). A ported command mis-translates silently, so `--help` states the contrast
   explicitly and T13 must carry a migration note.
3. **Artifact required.** After a zero-exit coverage command we require the LCOV file and exit `1`
   otherwise. crap4go's `LoadProfile` returns `nil, nil` on a missing file — silently yielding an
   empty profile, 0% everywhere, and a confidently inflated report. Our stricter behaviour is a
   deliberate correctness improvement.

**Confirmed parity:** token-absent append is crap4go behaviour (we diverge per (3) — we run verbatim
and require the artifact rather than appending llvm-cov-specific flags; human-ruled). **Confirmed
addition:** crap4go has **no `--lcov-path` analogue**, so our precedence rule is parity-unconstrained.

**Precedence (human ruling):** `--lcov-path` wins over `--test-command`, decided **at the type level**
(`CoverageSource::Existing` ⇒ the runner is genuinely unreachable) — not a clap `conflicts_with`,
which would break the CI wrapper pattern. An **advisory** stderr line says `--test-command` was
ignored. A `--test-command` with no `{lcov}` token warns **upfront, before spawning**, rather than
failing five minutes later. Advisories never affect the exit code (C6).

### T8 review notes (Anders — approve-with-suggestions ×2, 2026-08-24)

T8 (`src/runner.rs` + `RunConfig`) opens **S2**. Bhaskar FAIL → fix → PASS → FAIL → fix → **PASS**
(88 tests: 72 lib + 16 CLI integration). He caught a **silently-wrong-answer bug**: zero-config
accepted a **stale** `target/crap4rust/coverage.lcov` from an earlier run and reported it as current.
Now impossible — `generate_coverage` clears the artifact before spawning and **requires** it after.
He also caught a weakened test assertion (`contains("error: ")`) that the spawned clap child satisfied
on its own; now three components matched by byte offset with ordering asserted.

Design shape: `enum CoverageSource { Existing(PathBuf), Generated{command, artifact} }` encodes
**ownership** of the LCOV file, making "never delete the user's BYO file" a **type-level** property
rather than a runtime `if`. `GeneratedArtifact(PathBuf)` (private field, `clear()` the sole removal
path, constructible only in the `Generated` arm) means an `Existing` path is not type-compatible with
the deletion code. Artifact lifecycle lives in `runner`, not the composition root —
`fs::remove_file` never shares a module with the CLI. `run_with(cmd, spawn)` closure injection keeps
every test off a real `cargo llvm-cov`. `RunConfig::new` is parser-independent (FC-T7c discharged).

Forward constraints:
- **FC-T8a (artifact ownership is type-level).** Only `CoverageSource::Generated` may construct a
  `GeneratedArtifact`; `clear()` stays the sole removal path, private to `runner`. Any future flag
  naming an output location routes through `GeneratedArtifact` or gets no deletion rights. A
  user-supplied path is never deleted, ever.
- **FC-T8b (no real coverage tool in tests).** The injected `spawn` is the only way the failure
  taxonomy is asserted. Never invoke real `cargo llvm-cov` from a test (slow, environment-dependent,
  flaky — golden rule #8). CI runs ubuntu-latest **and** windows-latest.
- **FC-T8c (precedence stays type-level).** Decided once in `CoverageSource::resolve` and expressed by
  which variant is built — never re-checked downstream. The ignored-`--test-command` advisory is
  emitted, never escalated to an error (C6).
- **FC-T8d (divergence register).** See C17 — `--test-command` is **not** parity; record it as such,
  and do not let T13 claim otherwise.
- **FC-T8e (artifact path is ours).** `target/crap4rust/` is tool-owned, created by us, never assumed
  created by the coverage command. **T9:** once workspace roots are enumerated, decide one shared
  artifact vs per-root — a single `DEFAULT_LCOV_PATH` across roots lets later roots clobber earlier.
  Coverage is **workspace-wide, one run per invocation** (`cargo llvm-cov` is workspace-aware);
  discovery goes per-member, coverage generation does not.
- **FC-T8f (argv escape hatch — **RESOLVED (human, 2026-08-24): NOT BUILT**).** Repeatable
  `--test-command-arg` was the sanctioned answer to spaces-in-paths (the gap bites hardest on Windows,
  the very platform we chose argv for). **Human ruling: ship v1 without it** — T13 documents "no spaces
  in `--test-command`" as a known limit, with `--lcov-path` as the workaround. A shell variant
  (`sh -c`/`cmd /c`) is **rejected**: platform-split, injection surface, untestable on half the CI
  matrix. Revisit post-v1 if it bites a real user.
- **FC-T8g (advisory carrier; binds FC-T7e).** `RunConfig::advisories()` is pre-rendered
  `Vec<String>`, stderr-only, and **must be drained by every entrypoint before calling `cli::run`** —
  `main` does; a T11 JSON frontend must too, or resolution warnings vanish. Advisories are facts about
  **configuration** (known at resolution time, must survive `Err`); `RunOutput.diagnostics` are facts
  about the **run** (only exist on `Ok`) — hence the deliberate split, with FC-T7b intact. If T11 rules
  warnings belong inside the JSON document, `advisories` **and** `diagnostics` convert to a structured
  type **together**, before `schema_version` freezes. Do not convert one alone.

### T9 review notes (Bhaskar FAIL ×3 → PASS; Anders — approve-with-suggestions, 2026-08-24)

T9 replaces the T7 `src`-scan placeholder with a **rustc-style module-graph BFS**. `cargo metadata`
enumerates members and their **product targets**; from each target's `src_path` the walk resolves
`mod` declarations rather than scanning directories. `Scope{segments, dir, relative}` models rustc's
`DirOwnership::Owned{relative}` — a crate root and a `mod.rs` own their directory, a file module
`foo.rs` gets `relative = Some("foo")`. Names come from the graph (`scope.segments.join("::")`),
**never** from the filesystem. Orphan files unreachable from a root are not analysed; nonstandard
roots (`cmd/tool.rs`) enumerate correctly; `lib.rs`/`main.rs`/`mod.rs` naming falls out with no
special-casing. `discover` returns `Discovery{sources, diagnostics}`, prepended to `RunOutput`.

**Three defect rounds, all silently-wrong-answer or worse:**
1. **Many-to-one LCOV attribution** — per-file resolution structurally cannot see two sources claiming
   one key (each query has one candidate, so never a tie). One member reported **another member's**
   coverage as a confident number. Fixed by a whole-join claim map in `join::join` ⇒
   `Attribution{Resolved, Collision}`; contested files report N/A with all claimants named.
2. **Orphans, nonstandard roots, per-package-only dedup** — the original directory walk analysed files
   rustc never compiles and silently omitted modules under nonstandard roots. Fixed by the rewrite.
3. **Unbounded `#[path]` recursion — a hang.** `visited` keyed on the raw `PathBuf`, so
   `#[path = "../src/lib.rs"] mod again;` produced `src/lib.rs`, `src/../src/lib.rs`, … — endlessly
   distinct keys reading one file. Now keyed on `fs::canonicalize` identity. A hang is worse than a
   wrong number: there is no output to inspect.

**Decline policy.** `#[cfg_attr(windows, path = "windows.rs")] mod imp;` selects its source file by a
cfg set we do not evaluate. We **decline**: the module and its whole subtree are unmeasured and stderr
says so. Analysing the default would emit ordinary rows for a file rustc may never compile —
indistinguishable from correct output in the frozen C14 table. Standing bar: **we may decline to
answer, but never answer confidently and wrongly.** Canonicalization failure on a queued file is an
**operational error (exit 1)**, not a decline — same class as an unreadable file, and the only bounded
answer. All three advisory conditions (missing module file naming both candidates; `foo.rs` **and**
`foo/mod.rs` both present; conditional path) are exit-code-neutral per C6.

Forward constraints:
- **FC-T9a (module/name semantics — settle before `schema_version` freezes).** `module` and `name`
  split the module path at **different points** for inline vs. file modules: `src/foo/inner.rs` gives
  `module = "demo::foo::inner"`, `name = "bar"`, but `mod inner { fn bar() }` inline in `src/foo.rs`
  gives `module = "demo::foo"`, `name = "inner::bar"`. Concatenated they are always right;
  individually **neither field has a stable meaning**. Invisible in the table, fatal in JSON.
  Blocking for T11, not for T9.
- **FC-T9b (per-row coverage caveat).** `coverage: null` cannot distinguish *absent from profile* /
  *ambiguous* / *contested* — three different user actions. `JoinedFunction` must carry its file's
  attribution before T11; T11 must **not** re-join attributions to rows by path string.
- **FC-T9c (declined work is in-document).** A declined subtree is simply absent from `functions[]`,
  so a machine consumer computing workspace CRAP is silently wrong. T11's JSON carries a structured
  top-level `warnings[]`.
- **FC-T9d (one diagnostic type, one sink; binds FC-T7e/FC-T8g).** Advisories + diagnostics become a
  single `Diagnostic{code, severity, phase, site, kind}` with `Display` preserving today's bytes,
  drained by `main` **on `Err` as well as `Ok`** — today discovery diagnostics are **lost** when a
  later stage fails, so a workspace with three unresolvable modules *and* one unparseable file prints
  only the error. Convert together, never one alone. Land before T11.
- **FC-T9e (query key ≠ display path).** `workspace_relative` emits `../shared/tool.rs` for members
  above the root; `suffix_overlap` matches on segments including the literal `".."`, which no absolute
  LCOV key can end with — so out-of-root members are **permanently N/A**. One string is doing two
  jobs (C15 display identity **and** LCOV query key). FC-T7a binds the *reported* path, not the
  *query*. Resolve at T10/T11 via `SourceFile::absolute` or a `..`-stripped query key.
- **FC-T9f (evidence ranking in collisions — human ruling).** `claimants.len() > 1` ⇒ `Collision`
  regardless of *how* each claimant matched, so one sloppy `Suffix` claim demotes another file's
  `Exact` match to N/A — discarding `Resolution` evidence at the moment it is most valuable.
- **FC-T9g (T10 filters units, not just functions).** A file whose entire content is `#[cfg(test)]`
  yields zero functions but, if it survives as a `SourceUnit`, still **stakes a claim on an LCOV key**
  — manufacturing a `Collision` that N/As a real file, and emitting attribution diagnostics for a file
  with no rows. Drop the empty unit **before** the join. Also: missing-module diagnostics for
  `#[cfg(test)] mod tests;` become noise post-T10 — suppress or downgrade.
- **FC-T9h (single-parse streaming — land before T12).** Files are parsed **twice** (declarations,
  then measurement) by **two different modules**, with near-duplicate error contexts and two
  independent owners of "what is an operational error". Do not thread whole ASTs (peak memory = every
  `syn::File` at once); **stream** them — `complexity::analyze_file(&syn::File)` already exists and
  `workspace` already holds the right AST at the right moment. One parse per file, peak memory one
  AST; under T12 that is *J* ASTs bounded by `--jobs`. Defines the rayon unit of work, so doing it
  after T12 means writing the parallel structure twice.
- **FC-T9i (enumeration stays sequential).** Graph BFS and workspace dedup are order-dependent by
  design (first claimant owns) — that is what makes FC-T6b determinism hold. Enumeration is I/O-bound
  `is_file`/`canonicalize` probes, not CPU. T12 parallelises **measurement only**, order-preserving
  `map`/`collect`, never `par_bridge`.
- **FC-T9j (PATH is a locator — human ruling).** Positional `PATH` no longer scopes analysis: it
  locates the workspace, and the whole workspace is analysed. Between T9 and T10 the tool
  **over-reports against user intent** — `crap4rust crates/alpha` now analyses everything. C5's
  path-fragment filters are the way back, but the positional is then overloaded as locator *and*
  filter, which is not defensible.
- **FC-T9k (split the two concerns).** `workspace.rs` fuses *workspace/target enumeration* (cargo's
  schema) with *module-graph resolution* (rustc's rules) in 1261 lines — two reasons to change.
  Suggested pure file split: `workspace/mod.rs` + `workspace/modgraph.rs`, resolver surface
  `walk(root, crate_ident, workspace_root)`. Do **not** abstract the filesystem behind a trait — the
  temp-tree `Tree` harness is the right fidelity; a mock FS would be YAGNI and less truthful.
- **FC-T9l (T13 documentation debt).** Orphan `.rs` files are not analysed (a **behaviour change**
  from S1's directory walk — a user may notice a file "disappear"); declined conditionally-pathed
  subtrees are unmeasured; `PATH` is a locator, not a scope; **the report may be silently incomplete
  without reading stderr**; out-of-root members report `..` paths and today N/A coverage; **on Windows,
  two workspace members on different drive letters cannot both be made root-relative** — see the
  documented limit at `src/workspace/mod.rs` (`workspace_relative`).

### C18–C21 — Human rulings on T9 (2026-08-24)

- **C18 — incomplete-report notice (resolves Anders §2).** When discovery declines any work, a single
  trailing stdout line is emitted after the table: `N modules not analysed (see stderr)`. Rationale:
  stderr is the wrong channel for a fact about the artifact when the artifact is the deliverable —
  `crap4rust . > report.txt` must not look complete when it is not. **A report that can be silently
  incomplete is a correctness property, not a formatting preference.** Additive: emitted only in the
  declined case, so the happy-path bytes are untouched and the nine byte-locked C14 tests stand. C14
  parity binds the **table**, not what follows it.
- **C19 — exact-wins collision ranking (resolves FC-T9f).** When a contested LCOV key has **exactly
  one** `Exact` claimant, that claimant wins and the `Suffix` claimants become `Unresolved` (each
  still diagnosed). Any other shape (0 exact, or 2+ exact) stays `Collision` ⇒ N/A with all claimants
  named. This is not a guess: an exact normalized match beating a suffix guess is **evidence**, and
  `Resolution` exists precisely to carry *how* a file matched. Strictly reduces spurious N/A.
- **C20 — module/name normalisation (resolves FC-T9a, option ii).** `module` is the function's **full**
  module path (file scope **+** inline `mod` segments); `name` is the bare (or `<Type as Trait>::`
  qualified) function name. Consequence: the C14 Function column changes for inline-mod functions
  (`inner::bar` → `bar`) and the Module column absorbs the segment. **Accepted** — crap4go has no
  nested modules, so parity does not bind here. The byte-locked C14 tests covering inline-mod cases
  are updated **once**, deliberately, as part of T9b; the format itself stays frozen.
- **C21 — T9b is inserted before T10 (resolves Anders §4).** FC-T9d and FC-T9h land **now**, not folded
  into later tasks. FC-T9d is a live bug (discovery diagnostics are lost whenever a later stage
  fails), and FC-T9h defines T12's unit of work — deferring it means writing the parallel structure
  twice. Both are behaviour-preserving apart from the bug fix.

### T9b review notes (Bhaskar PASS first round; Anders — approve-with-suggestions, 2026-08-24)

Anders: *"the best-shaped commit on the branch."* The lost-warnings bug is fixed structurally, not
patched, and FC-T9h landed in its strongest form — `complexity::analyze_str` is `#[cfg(test)]`-only,
so **single-parse is a property, not a convention**. `Diagnostic::new` as the sole construction path
means `code`/`severity` cannot disagree with `kind`; per-variant `code` hand-written in a `match`
(not derived from variant names) correctly decouples the wire id from Rust identifiers.

**`RunOutput` deleted — FC-T7b confirmed satisfied, in a stronger form.** The constraint's letter was
"one field per output stream, nothing else"; its intent was that the deferred gate (D1) cannot smuggle
counters or flags into the return type. `run(&RunConfig, &mut Vec<Diagnostic>) -> Result<String>`
satisfies it maximally: **there is no struct left to grow a third field.**

**`visited`/`seen` collapsed into one identity-keyed set** with the skip happening *before* parsing.
"One file is measured, named and diagnosed exactly once" now holds at a single choke point
(`visited.insert`) rather than across two collections plus `FileEntry.diagnostics` plumbing — and the
`#[path]` hang is guarded by the same line. Fewer invariants, one enforcement site.

**Enumeration order is now module-graph BFS**, not path-sorted — forced by streaming (you cannot sort
a stream without buffering, and buffering is what FC-T9h forbids). Anders: BFS is also the *truthful*
order, since we model rustc's module graph and graph order is the order the modelled thing has;
path-sorted order was an artifact of the directory walk we deleted. Determinism holds (Bhaskar
verified no filesystem enumeration order participates).

**C19 deviation accepted (driver ruling).** The human ruling said superseded claimants become
`Unresolved`; Dave used a new `Attribution::Superseded{key, winner}` because `Unresolved` renders
*"absent from the coverage profile"* — which would be **false**: the record exists and belongs to a
named file. Observable outcome is identical (N/A + diagnosed); only the wording is honest. Endorsed.

Forward constraints:
- **FC-T9b\* (per-row coverage caveat — SUPERSEDES FC-T9b).** `JoinedFunction` must carry its file's
  attribution before T11, and the caveat has **five** states, not three: `suffix` (scored, but
  possibly the **wrong file's numbers** — arguably more dangerous than a null, and not covered by the
  original FC-T9b), `absent`, `ambiguous`, `contested`, `superseded` (plus silent `exact`). Derive
  from **one** function on `Attribution` consumed by both the stderr path and the JSON reporter; T11
  must **not** re-join attributions to rows by path string.
- **FC-T9m (diagnostic wire shape — before `schema_version`).** Rule at T11 whether `warnings[]`
  entries are **flat** (`code`, `severity`, `phase`, `file`, `line`, `column`, `message`) with an
  explicitly non-contractual `data` object, or a **variant-shaped union**. Flat recommended: a union
  makes every future `Kind` variant a schema change. `code` strings are **append-only and never
  renamed** once emitted — byte-lock all ten now (today `every_kind_has_a_distinct_code` asserts
  *distinctness*, not values, so a variant rename plus a careless `code()` edit silently breaks every
  consumer). Note the real freeze question is the **payload**, not the field list: `Kind` variants
  carry structured particulars (`candidates`, `keys`, `claimants`, `winner`) that today exist only
  inside `Display`.
- **FC-T9n (the C18 notice is presentation).** `run` appends the notice to the rendered report. When
  T11 selects the reporter **inside** `run` (FC-T7b), the notice must move behind that selection or it
  **will be concatenated onto a JSON document**. JSON carries the declined count structurally
  (FC-T9c), never as trailing text; both must come from the one `Kind::declines_analysis` predicate.
- **FC-T9o (cross-referenced identities).** `Superseded.winner` names another row's `file`. It must be
  the identical normalized identity rows carry, or a consumer cannot follow the reference. Binds
  together with FC-T9e — a `..`-prefixed out-of-root member could appear as a `winner` matching no
  row's `file`.
- **FC-T9p (severity stays warning-only while C6 holds).** `severity` earns its place by reserving the
  JSON slot so the shape cannot change later. **Do not add `Severity::Error`** while C6 holds: a
  diagnostic that says "error" and exits 0 is worse than no severity field. An operational failure is
  an `Err`, and that is its correct home.
- **FC-T9g escalated.** Was "post-T10 stderr noise"; it is now a **stdout number**. A
  `#[cfg(test)] mod tests;` whose file is absent will inflate "N modules not analysed" **on the
  artifact itself**. Now the highest-value item in T10.
- **FC-T9i extended.** Under streaming, T12 must parallelise measurement over a **bounded window** of
  ASTs and must never buffer the workspace's ASTs to restore a sort order.
- **FC-T9q (T11 sorts `functions[]` explicitly).** The JSON array must be sorted by C14's ordering
  (FC-T6d) and must **not** inherit input order — otherwise BFS graph order silently becomes part of
  the schema contract.
- **FC-T9r (`#[allow(dead_code)]` register).** `Diagnostic::code` and `::phase` are dead until T11 —
  correct over premature plumbing, but track them on the T11 checklist the way FC-T4a tracked `band`.
- Deferred test debt (Anders, cheap): byte-lock all ten `code` strings; a file reachable from two
  targets declaring a missing submodule ⇒ exactly **one** diagnostic and a C18 count of **1**
  (converts the emergent dedup property back into a stated one and guards the C18 number);
  `Superseded` end-to-end at integration level to pin ordering.

### C22–C24 — Human rulings on T9b (2026-08-24)

- **C22 — C18 counts distinct modules (resolves Anders §"new wart").** `declined()` currently counts
  **diagnostics**, so two cfg-guarded declarations of one module count 2. It must **dedupe by module
  path**. The `cfg_attr` subtree case remains a **lower bound** — one decline drops an unknown-sized
  subtree — and T13 must state that the number is a lower bound, not an exact count. Land in T10.
- **C23 — `severity` is warning-only while C6 holds (confirms FC-T9p as binding).** `Severity::Error`
  **must not** be added. A diagnostic that says "error" and exits 0 is worse than no severity field at
  all; operational failures are `Err`, which is their correct home. `severity` earns its place solely
  by reserving the JSON slot so the wire shape cannot change later.
- **C24 — JSON `warnings[]` is flat with a non-contractual `data` object (resolves FC-T9m).** Entries
  are `{code, severity, phase, file, line, column, message}` plus an **explicitly open, explicitly
  non-contractual** `data` object carrying the particulars. Rationale: a variant-shaped union would
  make **every future `Kind` variant a schema change**, and `Kind` has grown from 0 to 10 variants in
  two tasks. `code` strings are **append-only and never renamed** once emitted; byte-lock all ten.
  Consumers branch on `code` and read `message`; anything in `data` is best-effort.

### C25–C29 — Human rulings on T10 (2026-08-24)

- **C25 — MSRV is a SOURCE-ONLY floor.** `rust-version = "1.82"` describes **our crate's source**, not the
  committed `Cargo.lock`; consumers regenerate. Discovered while implementing: the prior declaration
  was **already false** — the committed lock pins `clap_builder` requiring edition2024 (Cargo ≥ 1.85).
  CI `msrv` job (ubuntu-only) reads the floor from `Cargo.toml`, installs and **verifies** the
  toolchain, deletes the lock, regenerates with **stable** cargo under
  `CARGO_RESOLVER_INCOMPATIBLE_RUST_VERSIONS=fallback`, then `cargo +1.82 build --locked`. The
  comments must state precisely what this does **not** prove: test-only code (`cargo build` compiles
  no `#[cfg(test)]` items), non-Ubuntu targets, and unaided Cargo 1.82 resolution.
- **C26 — `--filter` is a REPORTING narrowing; the numbers are invariant under it.** Filtering changes
  **which rows appear, never their values**. This is a product promise, not an implementation detail.
  Consequence: the join must see **every** discovered unit so the claim map is complete; selection
  happens on rows, before formatting. Invariant to pin with a test: *for any file present in both
  runs, every reported value is identical filtered and unfiltered.*
- **C27 — a zero-row report says so on stdout.** One line, one shape, covering the typo'd filter, the
  over-narrow filter, the all-test-code workspace and the genuinely empty crate alike. Rationale
  (Anders): a redirected empty report is today indistinguishable from a clean one **for reasons
  unrelated to filters**, which shows the filter framing was the wrong place to attack it — this
  dissolves the unmatched-filter question rather than answering it. An unmatched filter stays
  **stderr-only**: stdout carries statements about completeness *relative to what was asked for*, and
  an unmatched filter means nothing was asked for.
- **C28 — JSON echoes the effective request (FC-T10a).** T11's document carries top-level
  `filters: [...]` (and arguably the coverage source). `--filter` is a new reason `functions[]` may be
  short and a consumer cannot otherwise distinguish "no risk" from "you narrowed it". Due **before**
  `schema_version` freezes: without it, a frozen v1 is not self-describing.
- **C29 — filters stay case-sensitive on every platform, including Windows.** T13 documents the
  surprise. A "did you mean" hint is **rejected**: it invites fuzzy matching into a tool whose entire
  thesis is refusing to guess.

### T10 review notes (Bhaskar FAIL ×3 → PASS; Anders — approve-with-suggestions, 1 blocking → approved; **S3 closes**, 2026-08-24)

**Bhaskar rounds.**
1. **FAIL** — the empty-unit drop was broader than FC-T9g permits: it dropped legitimate **zero-function
   product** units, whose LCOV record was then silently consumed by another crate. The many-to-one
   attribution bug, **second appearance**. Also `#[cfg(test)] trait` methods still measured; integration
   fixtures leaked ~40 directories.
2. **FAIL** — whole-file test-only classification applied too late, so `#![cfg(test)] mod child;` still
   descended into the subtree. Declared MSRV unenforced.
3. **FAIL** (wording only) — the `msrv` job's comments overstated what it proves. Corrected → **PASS**.

**Anders — one blocking, B1.** `--filter` was applied **before** the join, so narrowing a report changed
the numbers in it: the claim map was incomplete and a filtered run could report another file's
coverage. The many-to-one bug, **third appearance** — and the first one Bhaskar missed. Fixed by joining
over all units and selecting rows afterwards (`retain_selected`, applied to `functions` **and**
`attributions`). Ruled C26. Bhaskar recorded the corrected heuristic: *whenever a stage drops or scopes
inputs, identify every later aggregation, attribution, dedup or ranking whose result depends on the
complete input set.*

**Endorsed.**
- `src/product.rs` — the single **pure** definition of "test code" (C5), consumed by `complexity` and
  `modgraph` alike. Anders: the best piece of design in this slice. Its stated rule — *we evaluate
  nothing whose truth depends on an environment we do not have* — is the T9 stance said once, in the
  one place it is decidable.
- The **funnel-gate refactor**: five per-kind checks collapsed to three gates at `syn`'s routing points
  (`visit_item`, `visit_impl_item`, `visit_trait_item`). `cfg` is not inherited, so a `#[cfg(test)]`
  container's members carry no evidence — the question must be asked of the **container**. The
  `_ => &[]` fallback fails toward **product**: over-report, never silent loss.
- `test`-decidability judged **principled, not convenient**: `cfg(test)` is the one cfg whose truth we
  know without an environment, because *cargo* sets it. (True of cargo, not rustc — `RUSTFLAGS --cfg
  test` can force it; T13 documents this.)
- B1's regression test carries two **named** invariants, not one over-broad equality: beta's diagnostic
  is byte-identical across runs (selected by site prefix in both), and the filtered-away unit's **own**
  diagnostic does not survive — asserted by absence, never by stderr line count. Counterparty phrases
  pin the filtered-away file in its *rendered attributed position*, which a bare `contains` could not
  (the LCOV key is the same string). Both sharpenings were mutation-proved, not argued.

**Forward constraints.**
- **FC-T10c (referential closure is not a schema guarantee — supersedes FC-T9o's wording).** Under
  `--filter`, `Superseded.winner` and `Collision.sources` may name files **absent from `functions[]`**.
  That is correct per C26 — attributions describe **whole-workspace** truth — but it means T11's
  document is **not referentially closed**. The schema must say so **where those fields are defined**.
  FC-T9o's requirement that the identity be byte-identical to a row's `file` still holds; its
  implication that a matching row always exists does not.
- **FC-T10d (two sections never merged).** The request echo (C28/FC-T10a) and the findings are distinct
  kinds of fact. Discriminator: *would this value differ if the same request produced a different
  artifact?* If no, it is request echo; if yes, it is a finding. Never let one drift into the other.
- **FC-T10e (JSON carries `declined` only — no redundant `rows`).** Corrects the "two structural
  fields" reading of FC-T9n: `functions.length` **is** the row count. Emitting a count beside the array
  creates a second source of truth that can disagree with the first.
- **FC-T10f (display-identity ≠ query-key is a STANDING invariant).** FC-T9e is
  **discharged-with-standing-invariant**, not closed. `query_key` strips leading `../` for the LCOV
  query **only**; the display path is untouched. Any future normalisation must preserve the split.
  Guarded at three sites (definition of `query_key`, cross-reference from `workspace_relative`, two
  tests pinning the pair) — keep all three.
- **Claimant ordering is contractual, not incidental.** `Collision.sources` is join-input order, which
  is the FC-T6b walk order (members by name; targets library-first then by name; files in module-graph
  order); verified end-to-end with no unordered collection in the chain. **T12 must preserve it** when
  rayon lands — the integration assertion says so out loud rather than failing mysteriously later.

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
