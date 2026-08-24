//! T7/T8 CLI exit-code integration tests (crap4go parity, C6).
//!
//! Drives the real binary so exit codes, stdout, and stderr are asserted as the
//! process actually emits them: `0` on success (even with high-CRAP functions),
//! `1` on operational error only, report on stdout, errors on stderr.
//!
//! Fixtures are written to a per-test subdirectory of the cargo-provided target
//! temp dir, so runs are deterministic and independent — no timing, no shared
//! state, no external tooling.

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

/// Create an isolated fixture directory containing `src/lib.rs` and
/// `coverage.lcov`, and return its path.
fn fixture(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("src")).expect("create fixture dir");
    fs::write(dir.join("src").join("lib.rs"), SOURCE).expect("write fixture source");
    fs::write(dir.join("coverage.lcov"), LCOV).expect("write fixture lcov");
    dir
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
risky                          src/lib.rs                             6    0.0%     42.0
covered                        src/lib.rs                             1  100.0%      1.0
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
risky                          src/lib.rs                             6    0.0%     42.0
covered                        src/lib.rs                             1  100.0%      1.0
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
