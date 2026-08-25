//! Diagnostics (FC-T9d) — one type, one sink.
//!
//! Everything the tool has to *say* — rather than report or fail on — is a
//! [`Diagnostic`]. Before T9b there were two carriers: `RunConfig::advisories()`
//! (facts about configuration) and `RunOutput.diagnostics` (facts about the
//! run), both pre-rendered `Vec<String>`, each drained on its own path. Two
//! channels meant the run-phase one was **lost** whenever a later stage failed:
//! a workspace with three unresolvable modules *and* one unparseable file
//! printed only the error, throwing away the three warnings that best explained
//! it.
//!
//! So there is one type and one sink now, and the distinction FC-T8g drew is
//! kept as **data** ([`Phase`]) rather than as a separate channel: config facts
//! are known before the run starts and are therefore emitted before it (a
//! coverage command that will never run is worth saying *before* a long test
//! run, not after), run facts are emitted as they are produced. Neither ever
//! affects the exit code (C6).
//!
//! **Rendering is the only thing this module does with them.** Diagnostics are
//! data: the CLI edge prints them, nothing here does. [`Display`](fmt::Display)
//! is byte-for-byte what the two `Vec<String>` carriers emitted before, so the
//! unification is not observable on stderr.
//!
//! `Serialize` is deliberately **not** implemented — the JSON contract and its
//! `warnings[]` are T11's (FC-T9c), and the field set here is what T11 will
//! serialize.

use std::fmt;

/// How serious a diagnostic is.
///
/// One variant, because today every diagnostic is advisory: nothing here ever
/// changes the exit code (C6), and an operational failure is an `Err`, not a
/// diagnostic. It is a field rather than a hardcoded `"warning: "` prefix so
/// that the severity is *data* (T11 serializes it) and the prefix has exactly
/// one source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Severity {
    Warning,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Warning => f.write_str("warning"),
        }
    }
}

/// Which half of the invocation a diagnostic is a fact about (FC-T8g).
///
/// [`Config`](Self::Config) facts are known once the arguments are resolved,
/// before anything is spawned or read; [`Run`](Self::Run) facts only exist once
/// the pipeline has run. That difference decides *when* they can be emitted,
/// which is why it survives the unification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    Config,
    Run,
}

/// Where a diagnostic points: a source file, and within it a declaration when
/// the diagnostic is about one rather than about the file as a whole.
///
/// `file` is always the workspace-relative, forward-slashed path the rest of
/// the pipeline knows the file by (D2/FC-T7a) — never an absolute or
/// drive-lettered one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Site {
    pub(crate) file: String,
    /// 1-based line, as rustc counts them. `None` together with `column` when
    /// the diagnostic is about the whole file.
    pub(crate) line: Option<usize>,
    /// 1-based column, as rustc counts them.
    pub(crate) column: Option<usize>,
}

impl Site {
    /// A whole-file site: the file is the subject, no declaration in it is.
    pub(crate) fn file(file: impl Into<String>) -> Self {
        Self {
            file: file.into(),
            line: None,
            column: None,
        }
    }

    /// The site of one declaration, pointed at the way rustc points at one.
    pub(crate) fn at(file: impl Into<String>, line: usize, column: usize) -> Self {
        Self {
            file: file.into(),
            line: Some(line),
            column: Some(column),
        }
    }
}

impl fmt::Display for Site {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.line, self.column) {
            (Some(line), Some(column)) => write!(f, "{}:{line}:{column}", self.file),
            _ => f.write_str(&self.file),
        }
    }
}

/// What a diagnostic is *about*, with the particulars it needs to be
/// actionable. Rendering one is [`Display`](fmt::Display); it never prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Kind {
    /// `--lcov-path` won outright, so the coverage command was never run
    /// (FC-T8c).
    IgnoredTestCommand,
    /// `--test-command` carries no LCOV token, so it must write the artifact
    /// itself — said before spawning, not after a long test run.
    TestCommandWithoutLcovToken {
        token: &'static str,
        artifact: String,
    },
    /// A `mod` declaration resolved to no file at all.
    ModuleFileMissing {
        module: String,
        candidates: Vec<String>,
    },
    /// A `mod` declaration has both candidate files; rustc rejects this, so
    /// which one the numbers describe must not be a guess.
    ModuleFileAmbiguous {
        module: String,
        candidates: Vec<String>,
        analysed: String,
    },
    /// A `#[cfg_attr(.., path = "..")]` module: which file rustc compiles
    /// depends on a cfg set we do not evaluate, so it is declined.
    ModulePathConditional { module: String, attribute: String },
    /// The file's coverage was found only by segment-wise suffix match.
    CoverageSuffixMatch { key: String },
    /// Several LCOV keys match the file equally well (FC-T5a).
    CoverageAmbiguous { keys: Vec<String> },
    /// The file is not in the coverage profile at all.
    CoverageAbsent,
    /// One LCOV record is claimed by several source files and no claim
    /// outranks the others, so it is attributed to none of them.
    CoverageContested { key: String, claimants: Vec<String> },
    /// This file's claim on an LCOV record lost to a claimant that matched it
    /// exactly (C19).
    CoverageSuperseded { key: String, winner: String },
}

impl Kind {
    /// Stable machine identifier, independent of the rendered wording. Carried
    /// so T11's `warnings[]` has something to key on that is not prose.
    fn code(&self) -> &'static str {
        match self {
            Self::IgnoredTestCommand => "ignored-test-command",
            Self::TestCommandWithoutLcovToken { .. } => "test-command-without-lcov-token",
            Self::ModuleFileMissing { .. } => "module-file-missing",
            Self::ModuleFileAmbiguous { .. } => "module-file-ambiguous",
            Self::ModulePathConditional { .. } => "module-path-conditional",
            Self::CoverageSuffixMatch { .. } => "coverage-suffix-match",
            Self::CoverageAmbiguous { .. } => "coverage-ambiguous",
            Self::CoverageAbsent => "coverage-absent",
            Self::CoverageContested { .. } => "coverage-contested",
            Self::CoverageSuperseded { .. } => "coverage-superseded",
        }
    }

    /// `true` when this diagnostic reports work the tool **declined**: a module
    /// (and, for a conditional path, its whole subtree) that is not in the
    /// report at all. That is what C18's stdout notice counts — a report which
    /// can be silently incomplete is a correctness problem, and stderr is the
    /// wrong channel for a fact about the artifact.
    ///
    /// A module resolved from two candidate files is *not* a decline: one of
    /// them was analysed. A coverage miss is not one either: the functions are
    /// reported, they just carry `N/A`.
    pub(crate) fn declines_analysis(&self) -> bool {
        matches!(
            self,
            Self::ModuleFileMissing { .. } | Self::ModulePathConditional { .. }
        )
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IgnoredTestCommand => f.write_str(
                "--lcov-path given; --test-command ignored, no coverage command was run",
            ),
            Self::TestCommandWithoutLcovToken { token, artifact } => write!(
                f,
                "--test-command has no {token} token; it must itself write LCOV to {artifact}"
            ),
            Self::ModuleFileMissing { module, candidates } => write!(
                f,
                "module `{module}` is declared but no file was found for it (looked for {}); \
                 it is not analysed",
                candidates.join(" and "),
            ),
            Self::ModuleFileAmbiguous {
                module,
                candidates,
                analysed,
            } => write!(
                f,
                "module `{module}` has files at both {}; {analysed} is analysed",
                candidates.join(" and "),
            ),
            Self::ModulePathConditional { module, attribute } => write!(
                f,
                "module `{module}` selects its source file conditionally (`{attribute}`); \
                 which file rustc compiles depends on the cfg set, which is not evaluated, \
                 so it is not analysed",
            ),
            Self::CoverageSuffixMatch { key } => write!(
                f,
                "no exact coverage entry; resolved by suffix match to LCOV entry {key}"
            ),
            Self::CoverageAmbiguous { keys } => write!(
                f,
                "coverage entry is ambiguous between {}; its functions report N/A",
                keys.join(", "),
            ),
            Self::CoverageAbsent => {
                f.write_str("absent from the coverage profile; its functions report N/A")
            }
            Self::CoverageContested { key, claimants } => write!(
                f,
                "LCOV entry {key} is claimed by {}; which file it covers cannot be proven, \
                 so their functions report N/A",
                claimants.join(", "),
            ),
            Self::CoverageSuperseded { key, winner } => write!(
                f,
                "LCOV entry {key} matches {winner} exactly, so it is attributed there; \
                 its functions report N/A"
            ),
        }
    }
}

/// One thing the tool has to say, as data.
///
/// Rendering is [`Display`](fmt::Display) and printing is the CLI edge's job:
/// nothing in the pipeline writes to a stream.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Public only because the binary crate drains the sink and prints it; the
/// fields stay crate-private, so the public surface is exactly `Display`.
pub struct Diagnostic {
    /// Stable machine identifier for [`kind`](Self::kind), for T11's
    /// `warnings[]` (FC-T9c). Not rendered — the human-readable form is
    /// `Display`.
    #[allow(dead_code)]
    pub(crate) code: &'static str,
    /// Advisory, always, today (C6).
    pub(crate) severity: Severity,
    /// Config or run (FC-T8g); read by T11, which groups them.
    #[allow(dead_code)]
    pub(crate) phase: Phase,
    /// Where it points, when it points somewhere.
    pub(crate) site: Option<Site>,
    /// What it is about.
    pub(crate) kind: Kind,
}

impl Diagnostic {
    /// A fact about the *configuration*, known before the run starts (FC-T8g).
    pub(crate) fn config(kind: Kind) -> Self {
        Self::new(Phase::Config, None, kind)
    }

    /// A fact about the *run*, at a site in the workspace.
    pub(crate) fn run(site: Site, kind: Kind) -> Self {
        Self::new(Phase::Run, Some(site), kind)
    }

    /// The one construction path, so `code` and `severity` can never disagree
    /// with `kind`.
    fn new(phase: Phase, site: Option<Site>, kind: Kind) -> Self {
        Self {
            code: kind.code(),
            severity: Severity::Warning,
            phase,
            site,
            kind,
        }
    }
}

impl fmt::Display for Diagnostic {
    /// `<severity>: [<site>: ]<kind>` — byte-for-byte what the two pre-rendered
    /// `Vec<String>` carriers emitted before T9b.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: ", self.severity)?;
        if let Some(site) = &self.site {
            write!(f, "{site}: ")?;
        }
        write!(f, "{}", self.kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_config_diagnostic_renders_without_a_site() {
        let diagnostic = Diagnostic::config(Kind::IgnoredTestCommand);
        assert_eq!(
            diagnostic.to_string(),
            "warning: --lcov-path given; --test-command ignored, no coverage command was run"
        );
        assert_eq!(diagnostic.phase, Phase::Config);
        assert_eq!(diagnostic.code, "ignored-test-command");
    }

    #[test]
    fn a_whole_file_site_renders_without_a_position() {
        let diagnostic = Diagnostic::run(Site::file("src/lib.rs"), Kind::CoverageAbsent);
        assert_eq!(
            diagnostic.to_string(),
            "warning: src/lib.rs: absent from the coverage profile; its functions report N/A"
        );
    }

    #[test]
    fn a_declaration_site_renders_file_line_and_column() {
        let diagnostic = Diagnostic::run(
            Site::at("src/lib.rs", 2, 5),
            Kind::ModuleFileMissing {
                module: "demo::absent".to_string(),
                candidates: vec!["src/absent.rs".to_string(), "src/absent/mod.rs".to_string()],
            },
        );
        assert_eq!(
            diagnostic.to_string(),
            "warning: src/lib.rs:2:5: module `demo::absent` is declared but no file was found \
             for it (looked for src/absent.rs and src/absent/mod.rs); it is not analysed"
        );
    }

    /// C18 counts declined *work*, not every warning: a module resolved from
    /// two candidates was analysed, and a coverage miss still reports its
    /// functions.
    #[test]
    fn only_declined_work_counts_as_not_analysed() {
        assert!(Kind::ModuleFileMissing {
            module: "demo::absent".to_string(),
            candidates: Vec::new(),
        }
        .declines_analysis());
        assert!(Kind::ModulePathConditional {
            module: "demo::imp".to_string(),
            attribute: "#[cfg_attr(windows, path = \"windows.rs\")]".to_string(),
        }
        .declines_analysis());
        assert!(!Kind::ModuleFileAmbiguous {
            module: "demo::foo".to_string(),
            candidates: Vec::new(),
            analysed: "src/foo.rs".to_string(),
        }
        .declines_analysis());
        assert!(!Kind::CoverageAbsent.declines_analysis());
    }

    /// Every kind has its own code: a shared one would make T11's `warnings[]`
    /// unable to tell two conditions apart.
    #[test]
    fn every_kind_has_a_distinct_code() {
        let kinds = [
            Kind::IgnoredTestCommand,
            Kind::TestCommandWithoutLcovToken {
                token: "{lcov}",
                artifact: "target/crap4rust/coverage.lcov".to_string(),
            },
            Kind::ModuleFileMissing {
                module: String::new(),
                candidates: Vec::new(),
            },
            Kind::ModuleFileAmbiguous {
                module: String::new(),
                candidates: Vec::new(),
                analysed: String::new(),
            },
            Kind::ModulePathConditional {
                module: String::new(),
                attribute: String::new(),
            },
            Kind::CoverageSuffixMatch { key: String::new() },
            Kind::CoverageAmbiguous { keys: Vec::new() },
            Kind::CoverageAbsent,
            Kind::CoverageContested {
                key: String::new(),
                claimants: Vec::new(),
            },
            Kind::CoverageSuperseded {
                key: String::new(),
                winner: String::new(),
            },
        ];
        let mut codes: Vec<&str> = kinds.iter().map(Kind::code).collect();
        let count = codes.len();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), count, "duplicate diagnostic codes: {codes:?}");
    }
}
