# Design

crap4rust — a Rust port of Uncle Bob's `crap4go`. It computes the **CRAP** metric (Change Risk
Anti-Patterns) per function for a Rust cargo workspace and reports the riskiest (complex + under-tested)
functions.

`CRAP(f) = cc² × (1 − cov)³ + cc` — where `cc` is cyclomatic complexity and `cov` is fractional line
coverage. Bands: **1–5 Low · 5–30 Moderate · 30+ High**.

## System overview

- **What it is:** a batch CLI (`crap4rust`), not a service. Run on demand or in CI; it prints a "CRAP
  Report" table (crap4go parity) or versioned JSON, then exits.
- **Who uses it:** Rust developers, CI pipelines, and coding agents wanting a change-risk read-out.
- **Core domains:** cyclomatic complexity (owned, via `syn`), line coverage (via `cargo-llvm-cov`
  LCOV), and the CRAP calculation that joins them per function.
- **v1 scope:** faithful crap4go behavior + JSON output. It is a **reporter**, not a gate — exit `0` on
  success (even with high-CRAP functions), `1` on operational error. The CRAP-threshold gate and
  baseline/delta scoping are deferred (future guardrail hook).

## Architecture

Clean Architecture; dependencies point **inward** toward pure domain logic. I/O lives at the edges.

```
        ┌─────────────────── CLI (clap) ───────────────────┐   edge / I/O
        │   arg parsing, orchestration, exit codes           │
        └───────┬───────────────┬───────────────┬───────────┘
                │               │               │
   ┌────────────▼──┐   ┌────────▼────────┐   ┌──▼──────────────┐   edges / adapters
   │ coverage adptr │   │ workspace/source │   │ reporters       │
   │ (cargo-llvm-cov│   │ discovery (cargo │   │ (table / JSON)  │
   │  run + LCOV     │   │  metadata, fs)   │   │                 │
   │  parse)         │   │                  │   │                 │
   └────────┬───────┘   └────────┬─────────┘   └──▲──────────────┘
            │                    │                │
            │            ┌───────▼────────┐       │
            └───────────►│  CRAP domain   │◄──────┘   pure core (no I/O)
                         │  + join logic  │
                         └───────▲────────┘
                                 │
                         ┌───────┴────────┐
                         │  CC engine     │   pure core (no I/O)
                         │  (syn visitor) │
                         └────────────────┘
```

**Dependency-flow rules**

- The **CC engine** (`syn` AST → cyclomatic complexity + function identity/spans) and the **CRAP
  domain** (metric arithmetic, band classification, span↔coverage join) are **pure** — no filesystem,
  process, or git access. They are unit-testable in isolation.
- **Adapters** (coverage runner/LCOV parser, workspace/source discovery, reporters) depend inward on
  the domain, never the reverse.
- The **CLI** composes everything and owns process concerns (args, exit codes, parallelism).

## Key components

- **CC engine** (`syn` visitor) — walks items/expressions, computes cyclomatic complexity per the
  locked counting rules (base +1; `if`/`else if`, `while`/`while let`, `for`, `loop`, each `match` arm
  except a lone catch-all, `&&`, `||`, `if let`/`let-else`, `?`, and match-arm guards each +1; closure
  decisions attributed to the enclosing fn). Source-level only — no macro expansion. Also derives
  function identity/naming (`<Type as Trait>::method`, inherent `Type::method`, free fns).
- **Coverage adapter** — runs `cargo llvm-cov` (zero-config default → `target/crap4rust/coverage.lcov`)
  or consumes a user-supplied LCOV via `--lcov-path`; parses `DA` records into per-file line-hit maps.
- **Workspace/source discovery** — resolves workspace members via cargo metadata; enumerates product
  source (excluding `#[cfg(test)]`/`#[test]` items and `tests/`/`benches/`/`examples/`); applies
  positional path-fragment filters.
- **CRAP domain** — `crap(cc, cov)`, band classifier, and the join that intersects a function's `syn`
  span lines with LCOV covered/total to produce `cov`.
- **Reporters** — human table (header "CRAP Report", 5 columns: Function · Module · CC · Cov% · CRAP,
  sorted by CRAP desc) and a versioned JSON contract (`schema_version`).
- **CLI** — `clap`-based; wires the pipeline; `rayon` parallelism (`-j/--jobs`, full CPUs default);
  exit codes `0` success / `1` operational error.

## Cross-cutting concerns

- **Config/secrets:** none required; no network, no credentials. Never hardcode any.
- **Persistence:** none beyond the coverage artifact under `target/`.
- **Observability:** human/JSON report is the primary output; errors to stderr.
- **Error handling:** `Result`-based; `anyhow`/`thiserror` at the edges; no `unwrap`/`expect`/`panic!`
  on reachable paths outside tests. Operational failures (bad args, coverage-command failure,
  unparseable LCOV) exit `1`.
- **Testing:** unit tests for CC counting and CRAP arithmetic (pure core); integration tests for the
  coverage-join and CLI exit codes/output.

## Conventions

Mirror the Project profile in `.github/copilot-instructions.md`:

- Rust 2021; least-privilege visibility (private by default; `pub(crate)` over `pub`).
- Keep the CRAP domain and CC engine pure (no I/O); push I/O to the edges.
- No `unsafe`. `cargo fmt` + `cargo clippy -D warnings` stay clean.
- Parity with crap4go for UX, table layout, and CRAP arithmetic; idiomatic Rust everywhere else. The
  authoritative in-flight design lives in `docs/features/001-crap4rust.md`.
