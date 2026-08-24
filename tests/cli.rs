//! T7 CLI exit-code integration tests (crap4go parity, C6).
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
fn missing_required_lcov_arg_exits_one_with_error_on_stderr() {
    let dir = fixture("bad_args");
    // `--lcov-path` is required at S1 (running coverage is T8/S2).
    let out = run_in(&dir, &["src"]);

    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout_of(&out), "");
    assert!(!stderr_of(&out).is_empty());
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
