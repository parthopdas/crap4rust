//! Workspace/source discovery adapter (T9) — the filesystem edge.
//!
//! `docs/design.md` draws this as its own adapter box; for S1 a minimal `.rs`
//! walk lived in [`crate::cli`] (FC-T7d). It lives here now, and it resolves
//! **workspace members via cargo metadata** (C3/A3) instead of walking whatever
//! directory it was pointed at. Cargo's schema is this file's concern; rustc's
//! module-resolution rules are [`modgraph`]'s (FC-T9k).
//!
//! Two things the rest of the pipeline gets from this module, both of which
//! need package identity and therefore cannot be derived downstream:
//!
//! 1. **Crate-qualified module names** (C3) — `demo::foo::bar` for
//!    `<demo>/src/foo/bar.rs`. The reporter is *told* the Module string; it
//!    never derives crate identity (FC-T6a).
//! 2. **Workspace-relative, forward-slashed paths** — the path each source file
//!    is known by from here on. `cargo llvm-cov` keys its LCOV `SF:` records by
//!    real source paths, so a workspace-relative query resolves the *right*
//!    member's entry instead of colliding with every other member's
//!    `src/lib.rs` (FC-T5a, FC-T7a).
//!
//! **Targets and module graphs, never directories.** Enumeration is driven by
//! cargo metadata's `targets` — the crates cargo actually builds — and, within
//! a target, by walking the **module graph** from its crate root the way rustc
//! resolves it ([`modgraph`]). A package can hold several crates (`src/lib.rs`
//! plus `src/bin/*.rs` plus an explicit `[[bin]] path = "cmd/tool.rs"`), each
//! with its own name and its own root file, and the crate identifier a module
//! name is qualified by is the **target's** name, not the package's.
//!
//! Directory scanning cannot answer either question correctly: it analyses
//! files no crate root ever reaches (a `.rs` file cargo never compiles is not
//! product source), and it cannot tell one bin's siblings from another's
//! without either mis-attributing files or silently dropping compiled ones.
//!
//! **Streaming, not collecting (FC-T9h).** [`discover`] does not return a list
//! of files: it *yields* each one, with the AST already parsed for it, to a
//! consumer callback. One parse per file, peak memory one `syn::File`, and no
//! second owner of "what is an operational error". Enumeration stays sequential
//! and order-dependent — first claimant owns (FC-T9i) — so T12 parallelises
//! measurement only, order-preserving.
//!
//! **Testability seam.** [`parse_metadata`] is pure — it takes the JSON text —
//! so the metadata shape is unit-tested without spawning cargo. Only
//! [`cargo_metadata`] spawns, and the end-to-end enumeration is exercised by
//! integration tests against hermetic on-disk fixture workspaces that have no
//! dependencies (`--no-deps` resolves nothing, so there is no network or
//! registry access — golden rule #8).
//!
//! **Scope.** `#[cfg(test)]`/`#[test]` item filtering and the positional
//! path-fragment filters are T10 (C5) and are deliberately not pre-built here.
//! In particular a `#[cfg(test)] mod tests;` declaration *is* followed: loading
//! a declared module is module-graph resolution (this task), while deciding
//! that its contents are test code and dropping them is filtering (T10). Doing
//! the cfg evaluation here would both pull T10 forward and be wrong on its own
//! terms — a `#[cfg(feature = "x")] mod` is product source under one feature
//! set and not under another, and we do not know the feature set.

mod modgraph;

use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context};
use serde::Deserialize;

use crate::diagnostic::Diagnostic;

/// Target kinds that are not product source (C5) — cargo builds them, but they
/// are tests, benchmarks, examples or build scripts.
const NON_PRODUCT_KINDS: [&str; 4] = ["test", "bench", "example", "custom-build"];

/// One product source file, handed to the consumer as it is discovered.
pub(crate) struct SourceFile<'a> {
    /// Crate-qualified module path for the file (C3), e.g. `demo::foo::bar`.
    pub(crate) module: String,
    /// The file's path relative to the workspace root, forward-slashed. This is
    /// the path the file is *known by*: it is what the coverage join queries
    /// and what the identity fields (C15) carry.
    pub(crate) path: String,
    /// The file's AST, parsed once by the walk that found it (FC-T9h).
    pub(crate) ast: &'a syn::File,
}

/// The slice of `cargo metadata --format-version 1` output we consume. Every
/// other field is ignored, so schema growth is not a break.
#[derive(Debug, Deserialize)]
struct Metadata {
    workspace_root: PathBuf,
    /// With `--no-deps` this is exactly the workspace members.
    packages: Vec<Package>,
}

/// One workspace member.
#[derive(Debug, Deserialize)]
struct Package {
    /// Only used to order members deterministically (FC-T6b); crate identity
    /// comes from the target, not from here.
    name: String,
    targets: Vec<Target>,
}

/// One crate cargo builds from a package: its name, what it is, and the file it
/// is rooted at.
#[derive(Debug, Deserialize)]
struct Target {
    name: String,
    kind: Vec<String>,
    src_path: PathBuf,
}

impl Target {
    /// `true` for the crates that are product source (C5).
    fn is_product(&self) -> bool {
        !self
            .kind
            .iter()
            .any(|kind| NON_PRODUCT_KINDS.contains(&kind.as_str()))
    }

    /// The crate identifier this target's modules are qualified by: the
    /// **target** name with `-` normalized to `_`, as rustc does. A package
    /// `my-app` with `[[bin]] name = "tool"` yields `tool`, never `my_app`.
    fn crate_ident(&self) -> String {
        self.name.replace('-', "_")
    }

    /// Sort key giving a deterministic, library-first target order (FC-T6b).
    /// Library-first matters because a module shared by a package's lib and its
    /// bin is attributed to whichever target comes first.
    fn order_key(&self) -> (u8, &str) {
        let rank = u8::from(self.kind.iter().any(|kind| kind == "bin"));
        (rank, self.name.as_str())
    }
}

/// Enumerate the product sources of every member of the workspace containing
/// `path`, yielding each one — with its parsed AST — to `visit`.
///
/// `path` only says *where* the workspace is: cargo searches it and its
/// ancestors for the manifest, exactly as it does for any cargo command, and
/// the whole workspace is then enumerated. Ordering is deterministic
/// (FC-T6b) — members by name, targets library-first then by name, files in
/// module-graph order.
///
/// Diagnostics are **pushed into `diagnostics` as they are produced**, so they
/// survive a later failure: a workspace with three unresolvable modules *and*
/// one unparseable file must still say all four things (FC-T9d).
pub(crate) fn discover(
    path: &Path,
    diagnostics: &mut Vec<Diagnostic>,
    visit: &mut dyn FnMut(SourceFile<'_>) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let dir = if path.is_dir() {
        path.to_path_buf()
    } else if path.is_file() {
        path.parent().unwrap_or(Path::new(".")).to_path_buf()
    } else {
        bail!("path not found: {}", path.display());
    };

    let metadata = parse_metadata(&cargo_metadata(&dir)?)?;
    source_files(&metadata, diagnostics, visit)
}

/// Ask cargo for the workspace metadata, from `dir`.
///
/// `--no-deps` keeps this to the workspace's own members and, more importantly,
/// skips dependency resolution entirely: no lockfile update, no registry, no
/// network. A failure here is an operational error (C6 ⇒ exit 1) — most often
/// "there is no cargo workspace here" (A3) — so cargo's own message is
/// surfaced rather than swallowed.
fn cargo_metadata(dir: &Path) -> anyhow::Result<String> {
    let output = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(dir)
        .output()
        .with_context(|| format!("failed to run `cargo metadata` in {}", dir.display()))?;

    if !output.status.success() {
        bail!(
            "`cargo metadata` failed in {} ({})\n{}",
            dir.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim_end()
        );
    }
    String::from_utf8(output.stdout).context("`cargo metadata` produced non-UTF-8 output")
}

/// Parse `cargo metadata` JSON. Pure — the unit-test seam for the metadata
/// shape (no cargo process involved).
fn parse_metadata(json: &str) -> anyhow::Result<Metadata> {
    serde_json::from_str(json).context("failed to parse `cargo metadata` output")
}

/// Yield the product sources of every target in the workspace, in a
/// deterministic order: members by name, targets library-first then by name,
/// each target's files in module-graph order (FC-T6b).
///
/// **Dedup spans the whole workspace, and lives in the walk.** Cargo lets a
/// target's `path` point anywhere, so two *packages* can name the same file,
/// and one file can also be reached from a package's lib and its bin. One
/// [`modgraph::Walker`] serves every target, and its identity guard is what
/// makes "whichever target reaches it first owns it — and so names it" true: a
/// later claim is never walked, so it is never measured, never reported and
/// never diagnosed a second time.
fn source_files(
    metadata: &Metadata,
    diagnostics: &mut Vec<Diagnostic>,
    visit: &mut dyn FnMut(SourceFile<'_>) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let mut packages: Vec<&Package> = metadata.packages.iter().collect();
    packages.sort_by(|a, b| a.name.cmp(&b.name));

    let workspace_root = &metadata.workspace_root;
    let mut walker = modgraph::Walker::new(workspace_root, diagnostics);
    for package in packages {
        let mut targets: Vec<&Target> = package.targets.iter().filter(|t| t.is_product()).collect();
        targets.sort_by(|a, b| a.order_key().cmp(&b.order_key()));

        for target in targets {
            walker.walk(&target.src_path, &target.crate_ident(), &mut |module| {
                visit(SourceFile {
                    module: module.module,
                    path: workspace_relative(workspace_root, module.file),
                    ast: module.ast,
                })
            })?;
        }
    }
    Ok(())
}

/// `file` as a forward-slashed path **relative** to the workspace root — the
/// key the coverage join queries by and the C15 identity `file` (FC-T7a).
///
/// A member outside the workspace root (a path dependency above it) gets a
/// lexical relative path with `..` segments. An absolute path — a drive letter
/// on Windows, a build-machine path anywhere — must never reach a report row,
/// so when the two paths share no prefix at all (different Windows drives, the
/// only case with no relative form) the path's root/prefix components are
/// dropped instead.
pub(super) fn workspace_relative(workspace_root: &Path, file: &Path) -> String {
    let root: Vec<Component> = workspace_root.components().collect();
    let target: Vec<Component> = file.components().collect();
    let common = root
        .iter()
        .zip(target.iter())
        .take_while(|(a, b)| a == b)
        .count();

    let segments: Vec<String> = if common == 0 {
        target
            .iter()
            .filter(|c| matches!(c, Component::Normal(_)))
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect()
    } else {
        std::iter::repeat_n("..".to_string(), root.len() - common)
            .chain(
                target[common..]
                    .iter()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned()),
            )
            .collect()
    };
    segments.join("/")
}

#[cfg(test)]
mod tests {
    use super::modgraph::tests::Tree;
    use super::*;

    /// A trimmed but realistic `cargo metadata --no-deps --format-version 1`
    /// document: two members, plus fields we must tolerate and ignore.
    const METADATA: &str = r#"{
      "packages": [
        {
          "name": "beta",
          "version": "0.1.0",
          "id": "path+file:///w/crates/beta#0.1.0",
          "manifest_path": "/w/crates/beta/Cargo.toml",
          "targets": [
            {"kind": ["lib"], "name": "beta", "src_path": "/w/crates/beta/src/lib.rs"},
            {"kind": ["test"], "name": "it", "src_path": "/w/crates/beta/tests/it.rs"}
          ]
        },
        {
          "name": "alpha",
          "version": "0.1.0",
          "id": "path+file:///w/crates/alpha#0.1.0",
          "manifest_path": "/w/crates/alpha/Cargo.toml",
          "targets": [
            {"kind": ["lib"], "name": "alpha", "src_path": "/w/crates/alpha/src/lib.rs"},
            {"kind": ["bin"], "name": "tool", "src_path": "/w/crates/alpha/src/bin/tool.rs"}
          ]
        }
      ],
      "workspace_members": [
        "path+file:///w/crates/beta#0.1.0",
        "path+file:///w/crates/alpha#0.1.0"
      ],
      "workspace_root": "/w",
      "target_directory": "/w/target",
      "version": 1,
      "resolve": null
    }"#;

    fn target(name: &str, kind: &str, src_path: &str) -> Target {
        Target {
            name: name.to_string(),
            kind: vec![kind.to_string()],
            src_path: PathBuf::from(src_path),
        }
    }

    /// Enumerate a whole "workspace" the way [`discover`] does — through the
    /// shared identity guard — from targets given as `(package, target, root)`,
    /// returning `(module, path)` per yielded file plus the diagnostic count.
    fn enumerate(tree: &Tree, targets: &[(&str, &str, &str)]) -> (Vec<(String, String)>, usize) {
        let packages = targets
            .iter()
            .map(|(package, name, root)| Package {
                name: (*package).to_string(),
                targets: vec![target(name, "lib", &tree.0.join(root).to_string_lossy())],
            })
            .collect();
        let metadata = Metadata {
            workspace_root: tree.0.clone(),
            packages,
        };

        let mut diagnostics = Vec::new();
        let mut files = Vec::new();
        source_files(&metadata, &mut diagnostics, &mut |source| {
            files.push((source.module, source.path));
            Ok(())
        })
        .expect("enumerate the workspace");
        (files, diagnostics.len())
    }

    #[test]
    fn metadata_json_yields_members_targets_and_workspace_root() {
        let metadata = parse_metadata(METADATA).expect("valid metadata");
        assert_eq!(metadata.workspace_root, PathBuf::from("/w"));
        let names: Vec<&str> = metadata.packages.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["beta", "alpha"]);
        assert_eq!(
            metadata.packages[1].targets[1].src_path,
            PathBuf::from("/w/crates/alpha/src/bin/tool.rs")
        );
    }

    #[test]
    fn malformed_metadata_is_an_error() {
        let err = parse_metadata("not json").unwrap_err();
        assert!(err.to_string().contains("cargo metadata"), "{err}");
    }

    #[test]
    fn non_product_target_kinds_are_excluded() {
        assert!(target("alpha", "lib", "/w/src/lib.rs").is_product());
        assert!(target("tool", "bin", "/w/src/bin/tool.rs").is_product());
        assert!(target("alpha", "proc-macro", "/w/src/lib.rs").is_product());
        for kind in NON_PRODUCT_KINDS {
            assert!(
                !target("x", kind, "/w/tests/x.rs").is_product(),
                "kind {kind} must not be product source"
            );
        }
    }

    /// Defect 3: dedup compares files, not spellings. Two spellings of one file
    /// must land in **one** identity domain, or the file is measured twice and
    /// the report silently reports a bigger product than exists.
    #[test]
    fn two_spellings_of_one_file_are_enumerated_once() {
        let tree = Tree::new("dedup");
        tree.write("src/lib.rs", "");

        let (files, diagnostics) = enumerate(
            &tree,
            &[
                ("alpha", "alpha", "src/lib.rs"),
                ("beta", "beta", "src/../src/lib.rs"),
            ],
        );

        assert_eq!(
            files,
            vec![("alpha".to_string(), "src/lib.rs".to_string())],
            "{files:?}"
        );
        assert_eq!(diagnostics, 0);
    }

    /// Diagnostics carry the same ownership as sources: one file reached from
    /// two targets declares one missing module, so exactly one warning is
    /// emitted — the second claim is never walked at all.
    #[test]
    fn a_file_reached_from_two_targets_warns_once() {
        let tree = Tree::new("shared_claim");
        tree.write("src/lib.rs", "mod absent;\n");

        let (files, diagnostics) = enumerate(
            &tree,
            &[
                ("alpha", "alpha", "src/lib.rs"),
                ("beta", "beta", "src/lib.rs"),
            ],
        );

        assert_eq!(files.len(), 1, "{files:?}");
        assert_eq!(diagnostics, 1);
    }

    #[test]
    fn library_targets_are_ordered_before_binaries() {
        let lib = target("zeta", "lib", "/w/src/lib.rs");
        let bin = target("alpha", "bin", "/w/src/main.rs");
        assert!(lib.order_key() < bin.order_key());
    }

    #[test]
    fn paths_are_workspace_relative_and_forward_slashed() {
        assert_eq!(
            workspace_relative(Path::new("/w"), Path::new("/w/crates/alpha/src/lib.rs")),
            "crates/alpha/src/lib.rs"
        );
        // On Windows the reported path is backslash-separated; the result is
        // forward-slashed either way, so it matches LCOV keys (see
        // `coverage::normalize_path`).
        #[cfg(windows)]
        assert_eq!(
            workspace_relative(Path::new(r"C:\w"), Path::new(r"C:\w\src\lib.rs")),
            "src/lib.rs"
        );
    }

    /// FC-T7a (defect 2): a member outside the workspace root must still yield
    /// a *relative* identity. An absolute path — and on Windows a drive
    /// letter — must never reach `SourceUnit.path` and from there
    /// `ReportRow.file`.
    #[test]
    fn a_member_outside_the_workspace_root_stays_relative() {
        assert_eq!(
            workspace_relative(Path::new("/w"), Path::new("/elsewhere/src/lib.rs")),
            "../elsewhere/src/lib.rs"
        );
        assert_eq!(
            workspace_relative(Path::new("/w/a/b"), Path::new("/w/x/src/lib.rs")),
            "../../x/src/lib.rs"
        );
        // Different Windows drives have no relative form at all; the prefix is
        // dropped rather than leaked.
        #[cfg(windows)]
        assert_eq!(
            workspace_relative(Path::new(r"C:\w"), Path::new(r"D:\other\src\lib.rs")),
            "other/src/lib.rs"
        );
    }

    #[test]
    fn missing_path_is_an_error() {
        let err = discover(Path::new("no/such/directory"), &mut Vec::new(), &mut |_| {
            Ok(())
        })
        .unwrap_err();
        assert!(err.to_string().contains("path not found"));
    }
}
