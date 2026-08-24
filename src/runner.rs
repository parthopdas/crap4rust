//! Coverage runner adapter (T8) — the process half of the coverage edge.
//!
//! `docs/design.md` draws one "coverage adapter" box covering *running*
//! `cargo llvm-cov` and *parsing* its LCOV. Parsing lives in [`crate::coverage`]
//! and is pure; spawning a child process is I/O, so it lives here, at the edge.
//! The CLI composes the two — it never builds a [`Command`] itself, and the
//! pure core never learns that a process existed.
//!
//! **The whole artifact lifecycle is adapter policy.** Choosing where LCOV
//! comes from ([`CoverageSource`]), creating the artifact directory, removing a
//! stale artifact, spawning, and requiring the artifact afterwards all live
//! here rather than in the CLI: they are how *this* adapter produces coverage,
//! not how a run is composed. The CLI only asks for an LCOV path to read.
//!
//! **Testability seam.** [`run`] is a thin wrapper over [`run_with`], which
//! takes the spawn function as a parameter. Unit tests drive `run_with` with a
//! stub that returns a canned [`Output`], so the failure taxonomy is asserted
//! without ever invoking a real `cargo llvm-cov` (slow, environment-dependent,
//! flaky — golden rule #8).
//!
//! **Errors are operational (C6 ⇒ exit 1).** A coverage command that cannot be
//! spawned, or that exits non-zero, fails the run; its own stderr is surfaced
//! rather than swallowed, and every variant names `--lcov-path` as the
//! bring-your-own-LCOV escape hatch (A1).

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{bail, Context};
use thiserror::Error;

/// The token replaced with the LCOV output path in a `--test-command` spec.
const LCOV_PLACEHOLDER: &str = "{lcov}";

/// Where the zero-config coverage run writes its LCOV artifact (C2).
pub(crate) const DEFAULT_LCOV_PATH: &str = "target/crap4rust/coverage.lcov";

/// Appended to every failure so a user without `cargo-llvm-cov` (A1) is told
/// what to do instead of being handed a raw io error.
const HINT: &str = "install it (`cargo install cargo-llvm-cov`) or run coverage yourself and pass the LCOV file with --lcov-path";

/// Why a coverage run failed. Every variant is an operational error (C6).
#[derive(Debug, Error)]
enum CoverageRunError {
    /// The program could not be found — the A1 case. The wording is
    /// path-agnostic because the program may equally be a bare name looked up
    /// on `PATH` or an explicit path that does not exist.
    #[error("coverage command `{command}` could not be found; {HINT}")]
    NotFound { command: String },
    /// The program ran but reported failure; its stderr is carried through.
    #[error("coverage command `{command}` failed ({status})\n{stderr}\nif coverage tooling is unavailable, {HINT}")]
    Failed {
        command: String,
        status: String,
        stderr: String,
    },
    /// The program could not be spawned for any other reason.
    #[error("failed to run coverage command `{command}`; {HINT}")]
    Spawn {
        command: String,
        #[source]
        source: io::Error,
    },
}

/// A resolved coverage command: a program plus its arguments, run directly —
/// there is no shell, so no quoting, globbing, piping or redirection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CoverageCommand {
    program: String,
    args: Vec<String>,
}

impl CoverageCommand {
    /// The zero-config default (C2): `cargo llvm-cov` writing LCOV to the
    /// artifact path the CLI then reads.
    fn cargo_llvm_cov(lcov_path: &Path) -> Self {
        Self {
            program: "cargo".to_string(),
            args: vec![
                "llvm-cov".to_string(),
                "--lcov".to_string(),
                "--output-path".to_string(),
                lcov_path.display().to_string(),
            ],
        }
    }

    /// Parse a `--test-command` spec: split on whitespace, *then* replace every
    /// occurrence of [`LCOV_PLACEHOLDER`] in each token with `lcov_path`. That
    /// order matters: substituting first would let an LCOV path containing
    /// spaces split into several arguments. Returns `None` for a blank spec (no
    /// tokens), which the caller treats as "not supplied".
    fn parse(spec: &str, lcov_path: &Path) -> Option<Self> {
        let out = lcov_path.display().to_string();
        let mut tokens = spec
            .split_whitespace()
            .map(|token| token.replace(LCOV_PLACEHOLDER, &out));
        let program = tokens.next()?;
        Some(Self {
            program,
            args: tokens.collect(),
        })
    }

    /// The spawnable form of this command.
    ///
    /// Kept on the type rather than inline in the spawn closure so anything
    /// else that needs the resolved child process — a dry run, a verbose echo —
    /// sees exactly what [`run`] would spawn.
    fn to_command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.args);
        command
    }
}

impl fmt::Display for CoverageCommand {
    /// The command as invoked, for error messages — informational, not a
    /// shell-safe round trip.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.program)?;
        for arg in &self.args {
            write!(f, " {arg}")?;
        }
        Ok(())
    }
}

/// An LCOV artifact this tool produced, and is therefore allowed to delete.
///
/// The newtype exists so the removal code is *unreachable* for a
/// bring-your-own file: only [`CoverageSource::Generated`] can construct one,
/// and deletion is exposed only as a method on it, so a `--lcov-path`
/// [`PathBuf`] is not even type-compatible with the code that removes files.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct GeneratedArtifact(PathBuf);

impl GeneratedArtifact {
    /// Where the coverage command must leave its LCOV.
    fn path(&self) -> &Path {
        &self.0
    }

    /// Make the artifact directory exist and the artifact itself *not* exist.
    ///
    /// A stale artifact from an earlier run is a silently-wrong answer, so it
    /// is removed before the coverage command is spawned; whatever is read back
    /// afterwards can then only be what this run produced.
    fn clear(&self) -> anyhow::Result<()> {
        // The artifact directory is ours (`target/crap4rust`), so create it
        // rather than depending on the coverage command to do it.
        if let Some(parent) = self.0.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create coverage directory {}", parent.display())
            })?;
        }
        match fs::remove_file(&self.0) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err).with_context(|| {
                format!(
                    "failed to remove stale coverage artifact {}",
                    self.0.display()
                )
            }),
        }
    }
}

/// Where a run's LCOV comes from.
///
/// The two cases are kept apart *by type* because they have opposite ownership
/// rules: a [`CoverageSource::Existing`] file belongs to the user and is only
/// ever read, while a [`CoverageSource::Generated`] artifact belongs to this
/// tool and is cleared before each run.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CoverageSource {
    /// Bring-your-own LCOV (`--lcov-path`): read as-is, never written, never
    /// removed.
    Existing(PathBuf),
    /// Zero-config (C2): run `command`, which must leave LCOV at `artifact`.
    Generated {
        command: CoverageCommand,
        artifact: GeneratedArtifact,
    },
}

impl CoverageSource {
    /// Resolve where coverage comes from, plus any advisory lines for the user.
    ///
    /// `lcov_path` wins outright: it short-circuits the runner entirely, so
    /// `test_command` is ignored — which is worth saying out loud, because a CI
    /// wrapper that always passes `--test-command` gets no coverage run at all.
    /// A `--test-command` with no [`LCOV_PLACEHOLDER`] is likewise flagged
    /// *before* spawning, so a long test run cannot fail only at the end for
    /// want of an artifact. Both are advisory: they never affect the exit
    /// code (C6).
    pub(crate) fn resolve(
        lcov_path: Option<PathBuf>,
        test_command: Option<&str>,
    ) -> (Self, Vec<String>) {
        let mut advisories = Vec::new();
        let source = match lcov_path {
            Some(lcov_path) => {
                if test_command.is_some() {
                    advisories.push(
                        "warning: --lcov-path given; --test-command ignored, no coverage command was run"
                            .to_string(),
                    );
                }
                Self::Existing(lcov_path)
            }
            None => {
                let artifact = GeneratedArtifact(PathBuf::from(DEFAULT_LCOV_PATH));
                // A blank `--test-command` carries no program, so it falls back
                // to the zero-config default rather than spawning nothing.
                let command = test_command
                    .and_then(|spec| CoverageCommand::parse(spec, artifact.path()))
                    .unwrap_or_else(|| CoverageCommand::cargo_llvm_cov(artifact.path()));
                if test_command.is_some_and(|spec| !spec.contains(LCOV_PLACEHOLDER)) {
                    advisories.push(format!(
                        "warning: --test-command has no {LCOV_PLACEHOLDER} token; \
                         it must itself write LCOV to {}",
                        artifact.path().display()
                    ));
                }
                Self::Generated { command, artifact }
            }
        };
        (source, advisories)
    }

    /// Make an LCOV file exist and return its path.
    ///
    /// This is not a getter: for a [`Self::Generated`] source it **spawns the
    /// coverage command** — a child process that may run for minutes — and
    /// **deletes the generated artifact** beforehand so the file read back can
    /// only be this run's (see [`GeneratedArtifact::clear`]). A
    /// [`Self::Existing`] (bring-your-own) path is returned as-is: nothing is
    /// spawned and nothing is ever deleted.
    pub(crate) fn ensure_lcov(&self) -> anyhow::Result<&Path> {
        match self {
            Self::Existing(path) => Ok(path),
            Self::Generated { command, artifact } => {
                generate_coverage(command, artifact)?;
                Ok(artifact.path())
            }
        }
    }
}

/// Run `command` and leave behind an artifact *this* invocation produced.
///
/// The artifact is cleared before spawning and required to exist afterwards, so
/// a command that exits 0 without writing LCOV is an operational error
/// (C6 ⇒ exit 1) rather than a report computed from whatever happened to be
/// lying around.
fn generate_coverage(
    command: &CoverageCommand,
    artifact: &GeneratedArtifact,
) -> anyhow::Result<()> {
    artifact.clear()?;
    run(command)?;
    if !artifact.path().exists() {
        bail!(
            "coverage command `{command}` succeeded but produced no LCOV file at {}; \
             have it write LCOV there (the `{LCOV_PLACEHOLDER}` token expands to that path), \
             or pass an existing profile with --lcov-path",
            artifact.path().display()
        );
    }
    Ok(())
}

/// Run `command` to completion, failing the run unless it exits successfully.
///
/// The child's stdout and stderr are captured rather than inherited so a
/// successful run leaves the report the only thing on our streams, and a failed
/// one can quote the child's own diagnostics.
fn run(command: &CoverageCommand) -> Result<(), CoverageRunError> {
    run_with(command, |command| command.to_command().output())
}

/// [`run`] with the spawn step injected — the unit-test seam.
fn run_with(
    command: &CoverageCommand,
    spawn: impl FnOnce(&CoverageCommand) -> io::Result<Output>,
) -> Result<(), CoverageRunError> {
    let output = spawn(command).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            CoverageRunError::NotFound {
                command: command.to_string(),
            }
        } else {
            CoverageRunError::Spawn {
                command: command.to_string(),
                source,
            }
        }
    })?;

    if output.status.success() {
        return Ok(());
    }
    Err(CoverageRunError::Failed {
        command: command.to_string(),
        status: output.status.to_string(),
        stderr: String::from_utf8_lossy(&output.stderr)
            .trim_end()
            .to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::ExitStatus;

    /// An `ExitStatus` for `code`, without spawning anything. The raw encoding
    /// is platform-specific (Unix wait status vs Windows exit code), hence the
    /// `cfg`; both CI targets are covered.
    fn exit_status(code: u32) -> ExitStatus {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            ExitStatus::from_raw((code as i32) << 8)
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::ExitStatusExt;
            ExitStatus::from_raw(code)
        }
    }

    fn output(code: u32, stderr: &str) -> Output {
        Output {
            status: exit_status(code),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    fn lcov() -> PathBuf {
        PathBuf::from("target/crap4rust/coverage.lcov")
    }

    #[test]
    fn default_command_writes_lcov_to_the_artifact_path() {
        let command = CoverageCommand::cargo_llvm_cov(&lcov());
        assert_eq!(
            command.to_string(),
            format!("cargo llvm-cov --lcov --output-path {}", lcov().display())
        );
    }

    #[test]
    fn test_command_substitutes_the_lcov_placeholder() {
        let command =
            CoverageCommand::parse("my-tool --out {lcov} --lcov", &lcov()).expect("non-blank spec");
        assert_eq!(
            command,
            CoverageCommand {
                program: "my-tool".to_string(),
                args: vec![
                    "--out".to_string(),
                    lcov().display().to_string(),
                    "--lcov".to_string(),
                ],
            }
        );
    }

    #[test]
    fn test_command_without_a_placeholder_is_run_verbatim() {
        let command = CoverageCommand::parse("  my-tool   --lcov  ", &lcov()).expect("non-blank");
        assert_eq!(command.to_string(), "my-tool --lcov");
    }

    #[test]
    fn blank_test_command_has_no_program() {
        assert_eq!(CoverageCommand::parse("   ", &lcov()), None);
    }

    #[test]
    fn test_command_substitutes_a_placeholder_embedded_in_a_token() {
        // `--out={lcov}` is a very common CLI shape: the token is part of a
        // larger argument rather than an argument of its own.
        let command = CoverageCommand::parse("my-tool --out={lcov}", &lcov()).expect("non-blank");
        assert_eq!(
            command,
            CoverageCommand {
                program: "my-tool".to_string(),
                args: vec![format!("--out={}", lcov().display())],
            }
        );
    }

    #[test]
    fn an_lcov_path_containing_spaces_stays_a_single_argument() {
        // Locks the split-then-substitute order: substituting first would split
        // this path across three arguments and silently corrupt the argv.
        let spaced = PathBuf::from("target/my cov dir/coverage.lcov");
        let command = CoverageCommand::parse("my-tool --out {lcov}", &spaced).expect("non-blank");
        assert_eq!(
            command,
            CoverageCommand {
                program: "my-tool".to_string(),
                args: vec![
                    "--out".to_string(),
                    "target/my cov dir/coverage.lcov".to_string(),
                ],
            }
        );
    }

    #[test]
    fn lcov_path_short_circuits_the_coverage_runner_and_says_so() {
        // The bring-your-own-LCOV seam (A1): nothing to execute, even when a
        // test command is also supplied — which is advised, not failed, so a CI
        // wrapper that always passes `--test-command` keeps working (C6).
        let (source, advisories) =
            CoverageSource::resolve(Some(PathBuf::from("mine.lcov")), Some("my-tool"));
        assert_eq!(source, CoverageSource::Existing(PathBuf::from("mine.lcov")));
        assert_eq!(advisories.len(), 1, "{advisories:?}");
        assert!(
            advisories[0].contains("--test-command ignored"),
            "{advisories:?}"
        );
    }

    #[test]
    fn an_lcov_path_alone_is_silent() {
        let (_, advisories) = CoverageSource::resolve(Some(PathBuf::from("mine.lcov")), None);
        assert!(advisories.is_empty(), "{advisories:?}");
    }

    #[test]
    fn zero_config_resolves_cargo_llvm_cov_into_the_default_artifact_path() {
        let (source, advisories) = CoverageSource::resolve(None, None);
        assert_eq!(
            source,
            CoverageSource::Generated {
                command: CoverageCommand::cargo_llvm_cov(Path::new(DEFAULT_LCOV_PATH)),
                artifact: GeneratedArtifact(PathBuf::from(DEFAULT_LCOV_PATH)),
            }
        );
        assert!(advisories.is_empty(), "{advisories:?}");
    }

    #[test]
    fn test_command_replaces_the_default_coverage_command() {
        let (source, advisories) = CoverageSource::resolve(None, Some("my-tool --out {lcov}"));
        assert_eq!(
            source,
            CoverageSource::Generated {
                command: CoverageCommand::parse(
                    "my-tool --out {lcov}",
                    Path::new(DEFAULT_LCOV_PATH)
                )
                .expect("non-blank"),
                artifact: GeneratedArtifact(PathBuf::from(DEFAULT_LCOV_PATH)),
            }
        );
        assert!(advisories.is_empty(), "{advisories:?}");
    }

    #[test]
    fn a_test_command_without_the_lcov_token_is_flagged_before_spawning() {
        // Resolution happens before anything is run, so a five-minute test
        // command is told up front that it must write LCOV itself.
        let (_, advisories) = CoverageSource::resolve(None, Some("my-tool --lcov"));
        assert_eq!(advisories.len(), 1, "{advisories:?}");
        assert!(advisories[0].contains(LCOV_PLACEHOLDER), "{advisories:?}");
        assert!(advisories[0].contains(DEFAULT_LCOV_PATH), "{advisories:?}");
    }

    #[test]
    fn a_successful_command_is_not_an_error() {
        let command = CoverageCommand::cargo_llvm_cov(&lcov());
        assert!(run_with(&command, |_| Ok(output(0, "noise"))).is_ok());
    }

    #[test]
    fn a_failing_command_reports_its_command_status_and_stderr() {
        let command = CoverageCommand::cargo_llvm_cov(&lcov());
        let err = run_with(&command, |_| {
            Ok(output(101, "error: no such command: `llvm-cov`"))
        })
        .expect_err("non-zero exit is an operational error");

        let message = err.to_string();
        assert!(message.contains("cargo llvm-cov --lcov"), "{message}");
        assert!(message.contains("no such command"), "{message}");
        assert!(message.contains("--lcov-path"), "{message}");
        // The child's exit status is named, whatever its platform rendering.
        assert!(message.contains("101"), "{message}");
    }

    #[test]
    fn a_missing_program_names_the_lcov_path_escape_hatch() {
        let command = CoverageCommand::cargo_llvm_cov(&lcov());
        let err = run_with(&command, |_| {
            Err(io::Error::new(io::ErrorKind::NotFound, "not found"))
        })
        .expect_err("a missing program is an operational error");

        let message = err.to_string();
        // Path-agnostic: the program may be a bare name or an explicit path.
        assert!(message.contains("could not be found"), "{message}");
        assert!(message.contains("--lcov-path"), "{message}");
    }

    #[test]
    fn other_spawn_failures_are_reported_as_spawn_errors() {
        let command = CoverageCommand::cargo_llvm_cov(&lcov());
        let err = run_with(&command, |_| {
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied"))
        })
        .expect_err("a failed spawn is an operational error");
        assert!(matches!(err, CoverageRunError::Spawn { .. }), "{err}");
    }
}
