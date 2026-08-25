//! T7/T8/T9 CLI exit-code integration tests (crap4go parity, C6).
//!
//! Drives the real binary so exit codes, stdout, and stderr are asserted as the
//! process actually emits them: `0` on success (even with high-CRAP functions),
//! `1` on operational error only, report on stdout, errors on stderr.
//!
//! Fixtures are dependency-free cargo workspaces written to a per-test
//! subdirectory of the cargo-provided target temp dir, so runs are
//! deterministic and independent — no timing, no shared state, no network, and
//! no external tooling (`cargo metadata --no-deps` resolves nothing, and no
//! test ever invokes a real `cargo llvm-cov`, FC-T8b).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Fixture source. Line numbers matter — the LCOV below targets them:
///
/// ```text
///  1: fn covered() -> i32 {
///  2:     1
///  3: }
///  4:
///  5: fn risky(x: i32) -> i32 {   ← CC 6: base + if + && + 3 non-wildcard arms
///  6:     if x > 0 && x < 10 {
///  7:         return x;
///  8:     }
///  9:     match x {
/// 10:         0 => 0,
/// 11:         1 => 1,
/// 12:         2 => 2,
/// 13:         _ => -1,
/// 14:     }
/// 15: }
/// ```
const SOURCE: &str = "\
fn covered() -> i32 {
    1
}

fn risky(x: i32) -> i32 {
    if x > 0 && x < 10 {
        return x;
    }
    match x {
        0 => 0,
        1 => 1,
        2 => 2,
        _ => -1,
    }
}
";

/// `covered` [1..=3]: line 2 hit ⇒ 1/1 = 100%.
/// `risky` [5..=15]: lines 6,7,9..13 all uncovered ⇒ 0/7 = 0%.
const LCOV: &str = "\
SF:src/lib.rs
DA:2,3
DA:6,0
DA:7,0
DA:9,0
DA:10,0
DA:11,0
DA:12,0
DA:13,0
end_of_record
";

/// A profile that disagrees with [`LCOV`] on every uncovered line: read as if
/// fresh it makes `risky` fully covered (CRAP 6.0 instead of 42.0), so a report
/// computed from it is unmistakable.
const STALE_LCOV: &str = "\
SF:src/lib.rs
DA:2,3
DA:6,1
DA:7,1
DA:9,1
DA:10,1
DA:11,1
DA:12,1
DA:13,1
end_of_record
";

/// Create an isolated fixture directory containing a minimal cargo package
/// (`Cargo.toml` + `src/lib.rs`) and `coverage.lcov`, and return its path.
///
/// The manifest is what makes this a workspace cargo metadata can resolve (A3):
/// it declares its own `[workspace]`, so the fixture is self-contained rather
/// than being drawn into whatever workspace encloses the target directory, and
/// it has **no dependencies**, so `cargo metadata --no-deps` resolves nothing
/// and touches no registry or network.
fn fixture(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("src")).expect("create fixture dir");
    fs::write(dir.join("Cargo.toml"), manifest("demo")).expect("write fixture manifest");
    fs::write(dir.join("src").join("lib.rs"), SOURCE).expect("write fixture source");
    fs::write(dir.join("coverage.lcov"), LCOV).expect("write fixture lcov");
    dir
}

/// A dependency-free manifest for a package that is its own workspace root.
fn manifest(package: &str) -> String {
    format!(
        "[package]\nname = \"{package}\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[workspace]\n"
    )
}

/// Run the built binary with `dir` as its working directory.
fn run_in(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_crap4rust"))
        .current_dir(dir)
        .args(args)
        .output()
        .expect("run crap4rust")
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout is UTF-8")
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr is UTF-8")
}

/// A path inside `dir` that is guaranteed not to exist: the fixture directory
/// is freshly created with a known set of entries, so an absolute path to a
/// name never written there cannot resolve. Unlike a bare program name it is
/// never looked up on `PATH`, so it is independent of what happens to be
/// installed on the CI runner (ubuntu-latest or windows-latest).
fn absent_program(dir: &Path) -> String {
    let program = dir.join("no-such-coverage-tool");
    assert!(!program.exists(), "fixture must not contain {program:?}");
    program.display().to_string()
}

/// A shell-free coverage-command stub that *produces* the LCOV artifact, the
/// way a real coverage tool does: it copies the fixture's `coverage.lcov` to
/// the `{lcov}` path. Both forms are stock OS tooling on the CI runners and
/// neither involves `cargo llvm-cov`.
fn artifact_producing_command() -> &'static str {
    if cfg!(windows) {
        "powershell -NoProfile -Command Copy-Item coverage.lcov {lcov}"
    } else {
        "cp coverage.lcov {lcov}"
    }
}

#[test]
fn normal_run_prints_report_to_stdout_and_exits_zero() {
    let dir = fixture("normal_run");
    let out = run_in(&dir, &["--lcov-path", "coverage.lcov", "src"]);

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    assert_eq!(
        stdout_of(&out),
        "\
CRAP Report
===========
Function                       Module                                CC    Cov%     CRAP
----------------------------------------------------------------------------------------
risky                          demo                                   6    0.0%     42.0
covered                        demo                                   1  100.0%      1.0
"
    );
    assert_eq!(stderr_of(&out), "");
}

#[test]
fn high_crap_function_still_exits_zero() {
    // `risky` scores 6²·1 + 6 = 42.0 — well inside the High band (>30) — and
    // C6 says v1 reports rather than gates, so the exit code stays 0.
    let dir = fixture("high_crap");
    let out = run_in(&dir, &["--lcov-path", "coverage.lcov", "src"]);

    assert!(stdout_of(&out).contains("42.0"), "{}", stdout_of(&out));
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn zero_config_runs_the_coverage_command_and_reads_the_default_artifact() {
    // Replaces T7's `missing_required_lcov_arg_exits_one_*`: omitting
    // `--lcov-path` is no longer an argument error — it runs coverage (C2) and
    // reads `target/crap4rust/coverage.lcov`. The stub *creates* that artifact
    // during this invocation (nothing is pre-placed), so the report can only
    // have come from coverage this run produced — and no real `cargo llvm-cov`
    // is involved.
    let dir = fixture("zero_config");
    let artifact = dir.join("target").join("crap4rust").join("coverage.lcov");
    assert!(!artifact.exists());

    let out = run_in(
        &dir,
        &["--test-command", artifact_producing_command(), "src"],
    );

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    assert!(
        artifact.exists(),
        "the coverage command must have written it"
    );
    assert!(
        stdout_of(&out).contains("CRAP Report"),
        "{}",
        stdout_of(&out)
    );
    assert!(stdout_of(&out).contains("42.0"), "{}", stdout_of(&out));
}

#[test]
fn coverage_command_that_succeeds_without_writing_lcov_exits_one() {
    // Exiting 0 is not enough: with no LCOV at the expected path there is
    // nothing legitimate to report on, so this is an operational error
    // (C6 ⇒ exit 1). The stub is our own binary's `--version`: exit 0, no side
    // effects, no shell, every CI OS.
    let dir = fixture("no_artifact");
    let out = run_in(
        &dir,
        &[
            "--test-command",
            &format!("{} --version", env!("CARGO_BIN_EXE_crap4rust")),
            "src",
        ],
    );

    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout_of(&out), "");
    let stderr = stderr_of(&out);
    // The command carries no `{lcov}` token, so it was told up front — before
    // being spawned — that it must write LCOV itself.
    assert!(
        stderr.starts_with("warning: --test-command has no {lcov} token"),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains("error: coverage command `"),
        "no parent error line; stderr: {stderr}"
    );
    assert!(stderr.contains("produced no LCOV"), "stderr: {stderr}");
    assert!(
        stderr.contains("target/crap4rust/coverage.lcov"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("--lcov-path"), "stderr: {stderr}");
}

#[test]
fn a_stale_generated_artifact_is_never_reused() {
    // The silently-wrong-answer case: an artifact left by an *earlier* run must
    // never be read as if this run had produced it. It is removed before the
    // coverage command is spawned, so a command that writes nothing fails the
    // run instead of yielding a full report computed from stale numbers.
    let dir = fixture("stale_artifact");
    let artifact = dir.join("target").join("crap4rust").join("coverage.lcov");
    fs::create_dir_all(artifact.parent().expect("artifact has a parent"))
        .expect("create artifact dir");
    fs::write(&artifact, LCOV).expect("pre-place a stale artifact");

    let out = run_in(
        &dir,
        &[
            "--test-command",
            &format!("{} --version", env!("CARGO_BIN_EXE_crap4rust")),
            "src",
        ],
    );

    assert_eq!(out.status.code(), Some(1), "stdout: {}", stdout_of(&out));
    assert_eq!(stdout_of(&out), "");
    let stderr = stderr_of(&out);
    assert!(stderr.contains("produced no LCOV"), "stderr: {stderr}");
    assert!(
        !artifact.exists(),
        "the stale artifact must have been removed"
    );
}

#[test]
fn a_stale_generated_artifact_is_replaced_by_a_successful_run() {
    // The positive half of stale-artifact protection: with a *succeeding*
    // coverage command, the report must come from the artifact this run
    // produced — proving the stale one was genuinely replaced, not merely
    // deleted. The pre-placed profile scores `risky` at 6.0; the fresh one
    // scores it at 42.0.
    let dir = fixture("stale_artifact_replaced");
    let artifact = dir.join("target").join("crap4rust").join("coverage.lcov");
    fs::create_dir_all(artifact.parent().expect("artifact has a parent"))
        .expect("create artifact dir");
    fs::write(&artifact, STALE_LCOV).expect("pre-place a stale artifact");

    let out = run_in(
        &dir,
        &["--test-command", artifact_producing_command(), "src"],
    );

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    assert_eq!(
        stdout_of(&out),
        "\
CRAP Report
===========
Function                       Module                                CC    Cov%     CRAP
----------------------------------------------------------------------------------------
risky                          demo                                   6    0.0%     42.0
covered                        demo                                   1  100.0%      1.0
"
    );
}

#[test]
fn coverage_command_failure_exits_one_with_its_stderr() {
    // A coverage-command failure is an operational error (C6 ⇒ exit 1). The
    // stub is our own binary given a bad flag: deterministic non-zero exit
    // with output on stderr, on every CI OS.
    let dir = fixture("coverage_failure");
    let out = run_in(
        &dir,
        &[
            "--test-command",
            &format!("{} --not-a-real-flag", env!("CARGO_BIN_EXE_crap4rust")),
            "src",
        ],
    );

    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout_of(&out), "");
    let stderr = stderr_of(&out);
    // Three components, in this order. The child is itself a clap program, so
    // its own stderr already contains a bare `error: ` — the parent line is
    // therefore matched by its own distinctive wording, which the child can
    // never emit, and the offsets pin the advisory-precedes-error property.
    let advisory = stderr
        .find("warning: --test-command has no {lcov} token")
        .unwrap_or_else(|| panic!("no upfront advisory; stderr: {stderr}"));
    let parent_error = stderr
        .find("error: coverage command `")
        .unwrap_or_else(|| panic!("no parent error line; stderr: {stderr}"));
    assert!(
        stderr[parent_error..].contains("` failed ("),
        "parent error is not the failure variant; stderr: {stderr}"
    );
    // The child's own diagnostics are surfaced, not swallowed.
    let child_diagnostic = stderr
        .find("unexpected argument")
        .unwrap_or_else(|| panic!("child diagnostic was swallowed; stderr: {stderr}"));
    assert!(
        advisory < parent_error && parent_error < child_diagnostic,
        "advisory ({advisory}) must precede the parent error ({parent_error}), \
         which must precede the child diagnostic ({child_diagnostic}); stderr: {stderr}"
    );
    assert!(stderr.contains("--not-a-real-flag"), "stderr: {stderr}");
}

#[test]
fn missing_coverage_tool_exits_one_with_the_lcov_path_hint() {
    // A1: an unrunnable coverage tool must produce an actionable message naming
    // the escape hatch, not a raw io `NotFound`. The program is an explicit
    // path inside this fixture, so its absence is guaranteed rather than
    // depending on the runner's `PATH`.
    let dir = fixture("missing_tool");
    let program = absent_program(&dir);
    let out = run_in(&dir, &["--test-command", &program, "src"]);

    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout_of(&out), "");
    let stderr = stderr_of(&out);
    assert!(stderr.contains(&program), "stderr: {stderr}");
    // Path-agnostic wording: this program *is* an explicit path, so "not found
    // on PATH" would be plainly untrue here.
    assert!(stderr.contains("could not be found"), "stderr: {stderr}");
    assert!(stderr.contains("--lcov-path"), "stderr: {stderr}");
}

#[test]
fn lcov_path_short_circuits_the_coverage_command() {
    // `--lcov-path` wins: the coverage command — a program guaranteed not to
    // exist — is never invoked, which is exactly why the run still succeeds.
    // That the command was ignored is said out loud on stderr, but it is purely
    // advisory: the exit code is still 0 (C6). The user's LCOV file is also
    // still there afterwards: a BYO file is only ever read, never removed.
    let dir = fixture("short_circuit");
    let out = run_in(
        &dir,
        &[
            "--lcov-path",
            "coverage.lcov",
            "--test-command",
            &absent_program(&dir),
            "src",
        ],
    );

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    assert!(
        stdout_of(&out).contains("CRAP Report"),
        "{}",
        stdout_of(&out)
    );
    let stderr = stderr_of(&out);
    assert_eq!(
        stderr,
        "warning: --lcov-path given; --test-command ignored, no coverage command was run\n"
    );
    assert!(dir.join("coverage.lcov").exists(), "BYO LCOV was removed");
}

#[test]
fn unknown_flag_exits_one() {
    // Also pins "no `--threshold` in v1" (C6/C11/D1).
    let dir = fixture("unknown_flag");
    let out = run_in(
        &dir,
        &["--lcov-path", "coverage.lcov", "--threshold", "5", "src"],
    );

    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout_of(&out), "");
}

#[test]
fn missing_lcov_file_exits_one_with_error_on_stderr() {
    let dir = fixture("missing_lcov");
    let out = run_in(&dir, &["--lcov-path", "no-such-file.lcov", "src"]);

    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout_of(&out), "");
    assert!(
        stderr_of(&out).contains("no-such-file.lcov"),
        "stderr: {}",
        stderr_of(&out)
    );
}

#[test]
fn unparseable_lcov_exits_one_with_error_on_stderr() {
    let dir = fixture("bad_lcov");
    fs::write(
        dir.join("coverage.lcov"),
        "SF:src/lib.rs\nDA:not-a-number,1\n",
    )
    .expect("write malformed lcov");
    let out = run_in(&dir, &["--lcov-path", "coverage.lcov", "src"]);

    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout_of(&out), "");
    assert!(
        stderr_of(&out).starts_with("error: "),
        "stderr: {}",
        stderr_of(&out)
    );
}

#[test]
fn missing_source_path_exits_one_with_error_on_stderr() {
    let dir = fixture("missing_source");
    let out = run_in(&dir, &["--lcov-path", "coverage.lcov", "no-such-dir"]);

    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout_of(&out), "");
    assert!(
        stderr_of(&out).contains("path not found"),
        "stderr: {}",
        stderr_of(&out)
    );
}

#[test]
fn unparseable_source_exits_one_with_error_on_stderr() {
    let dir = fixture("bad_source");
    fs::write(dir.join("src").join("lib.rs"), "fn broken( {").expect("write invalid source");
    let out = run_in(&dir, &["--lcov-path", "coverage.lcov", "src"]);

    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout_of(&out), "");
    assert!(
        stderr_of(&out).contains("failed to parse Rust source"),
        "stderr: {}",
        stderr_of(&out)
    );
}

#[test]
fn non_exact_path_match_warns_on_stderr_and_still_exits_zero() {
    // FC-T5b: the LCOV key is a longer path than the analysed source path, so
    // resolution succeeds only by segment-wise suffix match. That is reported
    // on stderr — once for the file — but the report is unchanged and the run
    // is still a success (C6).
    let dir = fixture("suffix_match");
    fs::write(
        dir.join("coverage.lcov"),
        LCOV.replace("SF:src/lib.rs", "SF:/build/proj/crates/demo/src/lib.rs"),
    )
    .expect("write suffix-keyed lcov");
    let out = run_in(&dir, &["--lcov-path", "coverage.lcov", "src"]);

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    // Coverage still resolves, so the numbers match the exact-match run.
    let stdout = stdout_of(&out);
    assert!(stdout.contains("CRAP Report"), "{stdout}");
    assert!(stdout.contains("42.0"), "{stdout}");
    assert!(stdout.contains("100.0%"), "{stdout}");

    let stderr = stderr_of(&out);
    // One diagnostic for the single source file — not one per function.
    assert_eq!(stderr.lines().count(), 1, "stderr: {stderr}");
    assert!(stderr.contains("suffix match"), "stderr: {stderr}");
    assert!(stderr.contains("lib.rs"), "stderr: {stderr}");
    assert!(
        stderr.contains("/build/proj/crates/demo/src/lib.rs"),
        "stderr: {stderr}"
    );
}

#[test]
fn unresolved_path_warns_on_stderr_and_still_exits_zero() {
    // FC-T5b: no LCOV key covers the analysed file, so its functions report
    // N/A (C13). The miss is diagnosed on stderr, not turned into a failure.
    let dir = fixture("unresolved_path");
    fs::write(
        dir.join("coverage.lcov"),
        LCOV.replace("SF:src/lib.rs", "SF:src/somewhere_else.rs"),
    )
    .expect("write non-matching lcov");
    let out = run_in(&dir, &["--lcov-path", "coverage.lcov", "src"]);

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("CRAP Report"), "{stdout}");
    assert!(stdout.contains("N/A"), "{stdout}");

    let stderr = stderr_of(&out);
    assert_eq!(stderr.lines().count(), 1, "stderr: {stderr}");
    assert!(stderr.contains("lib.rs"), "stderr: {stderr}");
    assert!(
        stderr.contains("absent from the coverage profile"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("N/A"), "stderr: {stderr}");
}

// ---------------------------------------------------------------------------
// T9 — workspace enumeration (C3/A3), crate-qualified modules, and the
// cross-member path collision guard (FC-T5a).
// ---------------------------------------------------------------------------

/// Root-package source. `alpha_one` spans lines 1..=3 with one instrumented
/// line (2), and CC = base 1 + `if` = 2.
const ALPHA_SOURCE: &str = "\
fn alpha_one(x: i32) -> i32 {
    if x > 0 { 1 } else { 0 }
}
";

/// Member source. `beta_one` spans lines 1..=3 with one instrumented line (2),
/// and CC = 1.
const BETA_SOURCE: &str = "\
fn beta_one() -> i32 {
    2
}
";

/// A two-member workspace: the root package `alpha` plus the member `beta`
/// under `crates/`. Both members have a `src/lib.rs` — the filename collision
/// that is guaranteed in any real workspace and that S1 was immune to only
/// because it analysed a single crate.
fn workspace_fixture(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&dir);
    let beta = dir.join("crates").join("beta");
    fs::create_dir_all(dir.join("src")).expect("create root src");
    fs::create_dir_all(beta.join("src")).expect("create member src");

    fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"alpha\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n\
         [workspace]\nmembers = [\"crates/beta\"]\n",
    )
    .expect("write workspace manifest");
    fs::write(dir.join("src").join("lib.rs"), ALPHA_SOURCE).expect("write root source");
    fs::write(beta.join("Cargo.toml"), manifest_member("beta")).expect("write member manifest");
    fs::write(beta.join("src").join("lib.rs"), BETA_SOURCE).expect("write member source");
    dir
}

/// A dependency-free manifest for a package that belongs to an enclosing
/// workspace (so it declares no `[workspace]` of its own).
fn manifest_member(package: &str) -> String {
    format!("[package]\nname = \"{package}\"\nversion = \"0.0.0\"\nedition = \"2021\"\n")
}

#[test]
fn every_workspace_member_is_analysed_with_crate_qualified_modules() {
    // C3/A3: members come from cargo metadata, not from walking the directory
    // that happened to be passed, and the Module column carries the
    // crate-qualified module name (FC-T6a) rather than S1's file path. The C14
    // layout is unchanged — only the content of the Module column moved.
    let dir = workspace_fixture("workspace_members");
    fs::write(
        dir.join("coverage.lcov"),
        "SF:src/lib.rs\nDA:2,0\nend_of_record\n\
         SF:crates/beta/src/lib.rs\nDA:2,1\nend_of_record\n",
    )
    .expect("write workspace lcov");

    let out = run_in(&dir, &["--lcov-path", "coverage.lcov", "."]);

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    assert_eq!(
        stdout_of(&out),
        "\
CRAP Report
===========
Function                       Module                                CC    Cov%     CRAP
----------------------------------------------------------------------------------------
alpha_one                      alpha                                  2    0.0%      6.0
beta_one                       beta                                   1  100.0%      1.0
"
    );
    // Both members' paths resolve exactly, so nothing is guessed.
    assert_eq!(stderr_of(&out), "");
}

#[test]
fn a_cross_member_path_collision_is_reported_as_ambiguous_not_guessed() {
    // FC-T5a: the LCOV was produced under a different build root, so no key
    // matches exactly. The root package's `src/lib.rs` is a segment-wise suffix
    // of *both* members' keys, equally well — "first key wins" would have
    // silently reported beta's coverage for alpha's function. Instead the tie
    // is refused: alpha reports N/A (C13) and the warning names both
    // candidates. beta's own path is longer and matches only one key, so it
    // still resolves. Neither affects the exit code (C6).
    let dir = workspace_fixture("workspace_collision");
    fs::write(
        dir.join("coverage.lcov"),
        "SF:/build/proj/src/lib.rs\nDA:2,0\nend_of_record\n\
         SF:/build/proj/crates/beta/src/lib.rs\nDA:2,1\nend_of_record\n",
    )
    .expect("write relocated lcov");

    let out = run_in(&dir, &["--lcov-path", "coverage.lcov", "."]);

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    assert_eq!(
        stdout_of(&out),
        "\
CRAP Report
===========
Function                       Module                                CC    Cov%     CRAP
----------------------------------------------------------------------------------------
beta_one                       beta                                   1  100.0%      1.0
alpha_one                      alpha                                  2    N/A       N/A
"
    );

    let stderr = stderr_of(&out);
    let lines: Vec<&str> = stderr.lines().collect();
    // One diagnostic per source file, in discovery (member-name) order.
    assert_eq!(lines.len(), 2, "stderr: {stderr}");
    assert!(lines[0].contains("src/lib.rs"), "stderr: {stderr}");
    assert!(lines[0].contains("ambiguous"), "stderr: {stderr}");
    // The tied candidates are named — an unqualified "ambiguous" is not
    // actionable.
    assert!(
        lines[0].contains("/build/proj/src/lib.rs"),
        "stderr: {stderr}"
    );
    assert!(
        lines[0].contains("/build/proj/crates/beta/src/lib.rs"),
        "stderr: {stderr}"
    );
    assert!(lines[0].contains("N/A"), "stderr: {stderr}");
    assert!(lines[1].contains("suffix match"), "stderr: {stderr}");
    assert!(
        lines[1].contains("/build/proj/crates/beta/src/lib.rs"),
        "stderr: {stderr}"
    );
}

#[test]
fn a_contested_record_goes_to_its_single_exact_claimant() {
    // C19: the profile holds a *single* record and both members' `src/lib.rs`
    // resolve to it — alpha's exactly, beta's by suffix. Those claims are not
    // equal evidence: the record's key *is* alpha's path, while beta merely
    // shares a suffix with it. So alpha is scored and beta is superseded —
    // still diagnosed, still N/A (C13), and told where the record went. Before
    // C19 the join discarded `Resolution` at exactly the moment it mattered and
    // both reported N/A. Still not an error (C6).
    let dir = workspace_fixture("workspace_many_to_one");
    fs::write(
        dir.join("coverage.lcov"),
        "SF:src/lib.rs\nDA:2,1\nend_of_record\n",
    )
    .expect("write single-record lcov");

    let out = run_in(&dir, &["--lcov-path", "coverage.lcov", "."]);

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    assert_eq!(
        stdout_of(&out),
        "\
CRAP Report
===========
Function                       Module                                CC    Cov%     CRAP
----------------------------------------------------------------------------------------
alpha_one                      alpha                                  2  100.0%      2.0
beta_one                       beta                                   1    N/A       N/A
"
    );

    let stderr = stderr_of(&out);
    let lines: Vec<&str> = stderr.lines().collect();
    // Only the loser is diagnosed: the winner's attribution is exact, and an
    // exact match has always been silent.
    assert_eq!(lines.len(), 1, "stderr: {stderr}");
    assert!(
        lines[0].contains(
            "crates/beta/src/lib.rs: LCOV entry src/lib.rs matches src/lib.rs exactly, \
             so it is attributed there"
        ),
        "stderr: {stderr}"
    );
    assert!(lines[0].contains("N/A"), "stderr: {stderr}");
}

#[test]
fn a_record_no_claimant_matches_exactly_is_a_collision_and_both_report_na() {
    // The other C19 shape, and the original defect-3 case: the single record's
    // key is a suffix of *both* members' paths and equal to neither, so nothing
    // ranks the claims. An LCOV path is relative to a build root we do not
    // know, so the record may describe either file: it is attributed to
    // neither, both report N/A (C13), and each warning names the contested key
    // and both claimants. Still not an error (C6).
    let dir = workspace_fixture("workspace_unranked_claims");
    fs::write(
        dir.join("coverage.lcov"),
        "SF:lib.rs\nDA:2,1\nend_of_record\n",
    )
    .expect("write single-record lcov");

    let out = run_in(&dir, &["--lcov-path", "coverage.lcov", "."]);

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    assert_eq!(
        stdout_of(&out),
        "\
CRAP Report
===========
Function                       Module                                CC    Cov%     CRAP
----------------------------------------------------------------------------------------
alpha_one                      alpha                                  2    N/A       N/A
beta_one                       beta                                   1    N/A       N/A
"
    );

    let stderr = stderr_of(&out);
    let lines: Vec<&str> = stderr.lines().collect();
    // One diagnostic per affected source file, in discovery order.
    assert_eq!(lines.len(), 2, "stderr: {stderr}");
    for line in &lines {
        // The contested record and *both* claimants, spelled out as a list so
        // that neither claimant can be satisfied by the LCOV key alone: the
        // many-to-one mapping is what has to be fixed, and neither line stands
        // alone without it.
        assert!(
            line.contains("LCOV entry lib.rs is claimed by src/lib.rs, crates/beta/src/lib.rs"),
            "stderr: {stderr}"
        );
        assert!(line.contains("N/A"), "stderr: {stderr}");
    }
}

// ---------------------------------------------------------------------------
// T9 — multi-target packages: enumeration and naming come from cargo metadata's
// `targets`, not from a `src/` walk.
// ---------------------------------------------------------------------------

/// A package holding three crates plus a member holding one, exercising every
/// way a target can differ from a `<package>/src` walk: a bin under `src/bin`,
/// an explicit `[[bin]] path` outside `src/` **with a sibling module**, a
/// target name that is not the package name, a `#[path]`-relocated module, and
/// two kinds of file no crate root reaches — one beside a library root, one in
/// a package that has no library at all.
fn multi_target_fixture(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&dir);
    let solo = dir.join("crates").join("solo");
    fs::create_dir_all(dir.join("src").join("bin")).expect("create root src/bin");
    fs::create_dir_all(dir.join("cmd")).expect("create root cmd");
    fs::create_dir_all(solo.join("cmd")).expect("create member cmd");
    fs::create_dir_all(solo.join("src")).expect("create member src");

    fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"alpha\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n\
         [[bin]]\nname = \"renamed\"\npath = \"cmd/tool.rs\"\n\n\
         [workspace]\nmembers = [\"crates/solo\"]\n",
    )
    .expect("write workspace manifest");
    fs::write(
        dir.join("src").join("lib.rs"),
        "#[path = \"aliased_file.rs\"]\nmod aliased;\n\nfn alpha_one() -> i32 {\n    1\n}\n",
    )
    .expect("write lib source");
    // Reached only through `#[path]`: its module name follows the graph
    // (`alpha::aliased`), not the file it happens to live in.
    fs::write(
        dir.join("src").join("aliased_file.rs"),
        "fn aliased_one() -> i32 {\n    6\n}\n",
    )
    .expect("write aliased source");
    // Beside a library root, and declared by nobody: cargo never compiles it.
    fs::write(
        dir.join("src").join("orphan.rs"),
        "fn orphan_one() -> i32 {\n    7\n}\n",
    )
    .expect("write library-dir orphan");
    fs::write(
        dir.join("src").join("bin").join("tool.rs"),
        "fn main() {}\n\nfn tool_one() -> i32 {\n    2\n}\n",
    )
    .expect("write src/bin source");
    // A nonstandard crate root owns its own directory, so `cmd/helper.rs` is
    // compiled product source of `renamed` and must be enumerated.
    fs::write(
        dir.join("cmd").join("tool.rs"),
        "mod helper;\n\nfn main() {}\n\nfn renamed_one() -> i32 {\n    3\n}\n",
    )
    .expect("write explicit-bin source");
    fs::write(
        dir.join("cmd").join("helper.rs"),
        "fn helper_one() -> i32 {\n    8\n}\n",
    )
    .expect("write sibling module source");

    fs::write(
        solo.join("Cargo.toml"),
        "[package]\nname = \"solo\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n\
         [[bin]]\nname = \"solo-tool\"\npath = \"cmd/main.rs\"\n",
    )
    .expect("write member manifest");
    fs::write(
        solo.join("cmd").join("main.rs"),
        "fn main() {}\n\nfn solo_one() -> i32 {\n    4\n}\n",
    )
    .expect("write member bin source");
    // Belongs to no cargo target: `solo` has no lib, so nothing builds this.
    fs::write(
        solo.join("src").join("stray.rs"),
        "fn stray_one() -> i32 {\n    5\n}\n",
    )
    .expect("write orphan source");
    dir
}

#[test]
fn every_target_is_enumerated_and_named_by_its_target_not_its_package() {
    // Defect 1: `src/bin/tool.rs` is the root of crate `tool` (not
    // `alpha::bin::tool`); an explicit `[[bin]] path` outside `src/` is a
    // target like any other and must not be missed, and its sibling module is
    // compiled source that must not be dropped either; a target name differing
    // from its package name is what the module is qualified by (`solo_tool`,
    // hyphen normalized); `#[path]` relocates a file without renaming its
    // module; and a `.rs` file no crate root declares is not analysed at all —
    // whether it sits beside a library root or in a package with no library.
    //
    // The whole report is asserted, not sampled: an exact match is the only
    // way a file analysed *twice* — the cross-target and cross-package dedup
    // failure — cannot slip through.
    let dir = multi_target_fixture("multi_target");
    fs::write(
        dir.join("coverage.lcov"),
        "SF:src/lib.rs\nDA:5,1\nend_of_record\n\
         SF:src/aliased_file.rs\nDA:2,1\nend_of_record\n\
         SF:src/bin/tool.rs\nDA:4,1\nend_of_record\n\
         SF:cmd/tool.rs\nDA:6,1\nend_of_record\n\
         SF:cmd/helper.rs\nDA:2,1\nend_of_record\n\
         SF:crates/solo/cmd/main.rs\nDA:4,1\nend_of_record\n",
    )
    .expect("write per-target lcov");

    let out = run_in(&dir, &["--lcov-path", "coverage.lcov", "."]);

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    assert_eq!(
        stdout_of(&out),
        "\
CRAP Report
===========
Function                       Module                                CC    Cov%     CRAP
----------------------------------------------------------------------------------------
aliased_one                    alpha::aliased                         1  100.0%      1.0
alpha_one                      alpha                                  1  100.0%      1.0
helper_one                     renamed::helper                        1  100.0%      1.0
main                           renamed                                1  100.0%      1.0
main                           tool                                   1  100.0%      1.0
main                           solo_tool                              1  100.0%      1.0
renamed_one                    renamed                                1  100.0%      1.0
solo_one                       solo_tool                              1  100.0%      1.0
tool_one                       tool                                   1  100.0%      1.0
"
    );
    // Every target's path resolves exactly, and every declared module was
    // found, so nothing is guessed and nothing is warned about.
    assert_eq!(stderr_of(&out), "");
}

/// Two packages naming the *same* file as a target root — which cargo permits,
/// since a target's `path` may point outside its package.
fn shared_source_fixture(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&dir);
    let beta = dir.join("crates").join("beta");
    fs::create_dir_all(dir.join("src")).expect("create root src");
    fs::create_dir_all(dir.join("shared")).expect("create shared dir");
    fs::create_dir_all(beta.join("src")).expect("create member src");

    fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"alpha\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n\
         [[bin]]\nname = \"alpha-shared\"\npath = \"shared/tool.rs\"\n\n\
         [workspace]\nmembers = [\"crates/beta\"]\n",
    )
    .expect("write workspace manifest");
    fs::write(dir.join("src").join("lib.rs"), ALPHA_SOURCE).expect("write root source");
    fs::write(
        dir.join("shared").join("tool.rs"),
        "fn main() {}\n\nfn shared_one() -> i32 {\n    9\n}\n",
    )
    .expect("write shared source");

    fs::write(
        beta.join("Cargo.toml"),
        "[package]\nname = \"beta\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n\
         [[bin]]\nname = \"beta-shared\"\npath = \"../../shared/tool.rs\"\n",
    )
    .expect("write member manifest");
    fs::write(beta.join("src").join("lib.rs"), BETA_SOURCE).expect("write member source");
    dir
}

#[test]
fn a_source_named_by_two_packages_is_analysed_exactly_once() {
    // Defect 3: dedup has to span the whole workspace, not one package's
    // targets, and it has to see through two different spellings of one file
    // (`shared/tool.rs` and `crates/beta/../../shared/tool.rs`). Analysed
    // twice, `shared_one` would be counted twice in every aggregate and
    // reported twice under two crate names.
    let dir = shared_source_fixture("shared_source");
    fs::write(
        dir.join("coverage.lcov"),
        "SF:src/lib.rs\nDA:2,1\nend_of_record\n\
         SF:crates/beta/src/lib.rs\nDA:2,1\nend_of_record\n\
         SF:shared/tool.rs\nDA:4,1\nend_of_record\n",
    )
    .expect("write lcov");

    let out = run_in(&dir, &["--lcov-path", "coverage.lcov", "."]);

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    // The first claimant in the deterministic order — package `alpha`, whose
    // bin target sorts before `beta`'s — owns and names the file.
    assert_eq!(
        stdout_of(&out),
        "\
CRAP Report
===========
Function                       Module                                CC    Cov%     CRAP
----------------------------------------------------------------------------------------
alpha_one                      alpha                                  2  100.0%      2.0
beta_one                       beta                                   1  100.0%      1.0
main                           alpha_shared                           1  100.0%      1.0
shared_one                     alpha_shared                           1  100.0%      1.0
"
    );
    assert_eq!(stderr_of(&out), "");
}

#[test]
fn a_declared_module_with_no_file_warns_and_still_exits_zero() {
    // A `mod` declaration that resolves to nothing is a real condition — the
    // crate does not compile as written, or the module is behind a cfg we do
    // not evaluate (T10). Either way it is *said*, naming both places rustc
    // would have looked, and the rest of the report is produced normally (C6).
    let dir = fixture("missing_module");
    fs::write(
        dir.join("src").join("lib.rs"),
        format!("mod absent;\n\n{SOURCE}"),
    )
    .expect("write source declaring a missing module");
    fs::write(
        dir.join("coverage.lcov"),
        LCOV.replace("DA:2,3", "DA:4,3").replace("DA:6,", "DA:8,"),
    )
    .expect("write shifted lcov");

    let out = run_in(&dir, &["--lcov-path", "coverage.lcov", "."]);

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("covered"), "{stdout}");
    assert!(stdout.contains("risky"), "{stdout}");

    let stderr = stderr_of(&out);
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 1, "stderr: {stderr}");
    assert!(lines[0].contains("src/lib.rs"), "stderr: {stderr}");
    assert!(lines[0].contains("demo::absent"), "stderr: {stderr}");
    assert!(lines[0].contains("src/absent.rs"), "stderr: {stderr}");
    assert!(lines[0].contains("src/absent/mod.rs"), "stderr: {stderr}");
}

/// C18: when discovery declines work, the *artifact* says so.
#[test]
fn a_single_declined_module_is_counted_on_stdout() {
    // `crap4rust . > report.txt` must not look complete when it is not: the
    // reader of the file never sees stderr, so a report missing a module has to
    // carry that fact itself. One module is "1 module", not "1 modules", and it
    // is separated from the table by a blank line — it is not a row, and a
    // row-shaped reader must not mistake it for data.
    let dir = fixture("declined_one");
    fs::write(
        dir.join("src").join("lib.rs"),
        format!("mod absent;\n\n{SOURCE}"),
    )
    .expect("write source declaring a missing module");
    fs::write(
        dir.join("coverage.lcov"),
        LCOV.replace("DA:2,3", "DA:4,3").replace("DA:6,", "DA:8,"),
    )
    .expect("write shifted lcov");

    let out = run_in(&dir, &["--lcov-path", "coverage.lcov", "."]);

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(
        stdout.ends_with("\n\n1 module not analysed (see stderr)\n"),
        "{stdout}"
    );
}

#[test]
fn several_declined_modules_are_counted_on_stdout() {
    // The count is of declined *modules*, and it is plural when it should be.
    // A conditionally-pathed module is declined too: which file rustc compiles
    // depends on a cfg set we do not evaluate.
    let dir = fixture("declined_several");
    fs::write(
        dir.join("src").join("lib.rs"),
        format!(
            "mod absent;\n\
             #[cfg_attr(windows, path = \"win.rs\")]\nmod imp;\n\n{SOURCE}"
        ),
    )
    .expect("write source declining two modules");
    fs::write(dir.join("coverage.lcov"), "SF:src/lib.rs\nend_of_record\n")
        .expect("write empty lcov");

    let out = run_in(&dir, &["--lcov-path", "coverage.lcov", "."]);

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(
        stdout.ends_with("\n\n2 modules not analysed (see stderr)\n"),
        "{stdout}"
    );
    assert_eq!(stderr_of(&out).lines().count(), 2, "{}", stderr_of(&out));
}

#[test]
fn a_complete_report_carries_no_notice() {
    // The notice is emitted *only* when work was declined, so the happy-path
    // bytes are untouched (C14).
    let dir = fixture("declined_none");
    let out = run_in(&dir, &["--lcov-path", "coverage.lcov", "."]);

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    assert!(
        !stdout_of(&out).contains("not analysed"),
        "{}",
        stdout_of(&out)
    );
}

/// C20: `module` is the function's full module path and `name` is its bare
/// name — for inline `mod`s as well as file modules.
#[test]
fn an_inline_module_names_the_function_in_the_module_column() {
    // Before C20 the two fields split at different points depending on how the
    // module was declared: a file module put its segment in Module, an inline
    // one glued it to Function (`inner::bar`), so neither field had a stable
    // meaning. The segment now moves to the Module column for both.
    let dir = fixture("inline_module");
    fs::write(
        dir.join("src").join("lib.rs"),
        "mod inner {\n    pub fn bar() -> i32 {\n        1\n    }\n}\n",
    )
    .expect("write source with an inline module");
    fs::write(
        dir.join("coverage.lcov"),
        "SF:src/lib.rs\nDA:3,1\nend_of_record\n",
    )
    .expect("write lcov");

    let out = run_in(&dir, &["--lcov-path", "coverage.lcov", "."]);

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    let stdout = stdout_of(&out);
    let row = stdout
        .lines()
        .find(|line| line.starts_with("bar "))
        .unwrap_or_else(|| panic!("no row for `bar`: {stdout}"));
    assert!(row.contains("demo::inner"), "{row}");
    assert!(!stdout.contains("inner::bar"), "{stdout}");
}

/// FC-T9d: diagnostics survive an `Err`.
#[test]
fn warnings_produced_before_a_failure_are_still_printed() {
    // Two channels meant the run-phase one was thrown away whenever a later
    // stage failed: this workspace has an unresolvable module *and* an
    // unparseable file, and printed only the error — losing the warning that
    // best explains the state the crate is in. One sink, drained on `Err` as
    // well as `Ok`, fixes it.
    let dir = fixture("warnings_before_failure");
    fs::write(dir.join("src").join("lib.rs"), "mod absent;\nmod broken;\n")
        .expect("write source declaring a missing and a broken module");
    fs::write(dir.join("src").join("broken.rs"), "fn broken( {\n")
        .expect("write unparseable source");

    let out = run_in(&dir, &["--lcov-path", "coverage.lcov", "."]);

    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr_of(&out));
    let stderr = stderr_of(&out);
    let lines: Vec<&str> = stderr.lines().collect();
    // The warning first — it is a fact about the workspace that was true before
    // the failure — then the error that ended the run.
    assert!(lines[0].starts_with("warning: "), "stderr: {stderr}");
    assert!(lines[0].contains("demo::absent"), "stderr: {stderr}");
    assert!(
        lines.iter().any(|line| line.starts_with("error: ")),
        "stderr: {stderr}"
    );
    // Nothing is reported: the run did not complete.
    assert_eq!(stdout_of(&out), "");
}

// ---------------------------------------------------------------------------
// T9 — FC-T7a: the path identity is relative, always.
// ---------------------------------------------------------------------------

#[test]
fn a_source_outside_the_workspace_root_is_reported_by_a_relative_path() {
    // FC-T7a (defect 2): a target rooted outside the workspace root — here an
    // explicit `[[bin]] path` above it, the shape a path-dependency layout also
    // produces — must still be known by a *relative* path. An absolute path — a
    // drive letter on Windows, a build-machine path anywhere — must never reach
    // the identity the join queries by and the report carries.
    let base = Path::new(env!("CARGO_TARGET_TMPDIR")).join("external_source");
    let _ = fs::remove_dir_all(&base);
    let root = base.join("workspace");
    fs::create_dir_all(root.join("src")).expect("create root src");
    fs::create_dir_all(base.join("shared")).expect("create external dir");

    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"alpha\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n\
         [[bin]]\nname = \"shared\"\npath = \"../shared/tool.rs\"\n\n\
         [workspace]\n",
    )
    .expect("write workspace manifest");
    fs::write(root.join("src").join("lib.rs"), ALPHA_SOURCE).expect("write root source");
    fs::write(
        base.join("shared").join("tool.rs"),
        "fn main() {}\n\nfn shared_one() -> i32 {\n    2\n}\n",
    )
    .expect("write external source");
    // Only the root package is in the profile, so the outside-the-root file's
    // path is named on stderr — which is where its identity becomes observable.
    fs::write(
        root.join("coverage.lcov"),
        "SF:src/lib.rs\nDA:2,1\nend_of_record\n",
    )
    .expect("write lcov");

    let out = run_in(&root, &["--lcov-path", "coverage.lcov", "."]);

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr_of(&out));
    // It is analysed, under its own target's name.
    assert!(
        stdout_of(&out).contains("shared_one"),
        "{}",
        stdout_of(&out)
    );

    let stderr = stderr_of(&out);
    let external_line = stderr
        .lines()
        .find(|line| line.contains("tool.rs"))
        .unwrap_or_else(|| panic!("external source not diagnosed; stderr:\n{stderr}"));
    assert!(
        external_line.contains("../shared/tool.rs"),
        "stderr: {stderr}"
    );
    // No absolute path and no Windows drive letter in the identity.
    assert!(
        !external_line.contains(":\\") && !external_line.contains(":/"),
        "an absolute path leaked: {external_line}"
    );
    assert!(
        !external_line.contains(" /"),
        "an absolute path leaked: {external_line}"
    );
}
