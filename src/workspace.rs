//! Workspace/source discovery adapter (T9) — the filesystem edge.
//!
//! `docs/design.md` draws this as its own adapter box; for S1 a minimal `.rs`
//! walk lived in [`crate::cli`] (FC-T7d). It lives here now, and it resolves
//! **workspace members via cargo metadata** (C3/A3) instead of walking whatever
//! directory it was pointed at.
//!
//! Two things the rest of the pipeline gets from this module, both of which
//! need package identity and therefore cannot be derived downstream:
//!
//! 1. **Crate-qualified module names** (C3) — `demo::foo::bar` for
//!    `<demo>/src/foo/bar.rs`. The reporter is *told* the Module string; it
//!    never derives crate identity (FC-T6a: this replaces the S1 path
//!    placeholder that blocked T11's `schema_version` freeze).
//! 2. **Workspace-relative, forward-slashed paths** — the path each source file
//!    is known by from here on. `cargo llvm-cov` keys its LCOV `SF:` records by
//!    real source paths, so a workspace-relative query resolves the *right*
//!    member's entry instead of colliding with every other member's
//!    `src/lib.rs` (FC-T5a, FC-T7a).
//!
//! **Targets and module graphs, never directories.** Enumeration is driven by
//! cargo metadata's `targets` — the crates cargo actually builds — and, within
//! a target, by walking the **module graph** from its crate root the way rustc
//! resolves it ([`target_sources`]). A package can hold several crates
//! (`src/lib.rs` plus `src/bin/*.rs` plus an explicit `[[bin]] path =
//! "cmd/tool.rs"`), each with its own name and its own root file, and the crate
//! identifier a module name is qualified by is the **target's** name, not the
//! package's.
//!
//! Directory scanning cannot answer either question correctly: it analyses
//! files no crate root ever reaches (a `.rs` file cargo never compiles is not
//! product source), and it cannot tell one bin's siblings from another's
//! without either mis-attributing files or silently dropping compiled ones.
//! The module graph answers both by construction, and it is also what makes a
//! module *name* right: `#[path]` decouples a file's location from its module
//! path, so the graph — not the path — is the truth.
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
//!
//! **One `cfg` construct is not deferrable, though.** `#[cfg(..)]` decides
//! *whether* a module is compiled; `#[cfg_attr(.., path = "..")]` decides
//! *which file* it is compiled from. Guessing the former over-reports at worst;
//! guessing the latter measures a file rustc never compiled and reports the
//! numbers as if it had. So a conditionally-pathed module is diagnosed and left
//! unanalysed rather than resolved to its default filename — we may decline to
//! answer, but never answer confidently and wrongly. Advisory only: the exit
//! code is unaffected (C6).

use std::collections::{BTreeSet, VecDeque};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context};
use serde::Deserialize;
use syn::punctuated::Punctuated;
use syn::Token;

/// Target kinds that are not product source (C5) — cargo builds them, but they
/// are tests, benchmarks, examples or build scripts.
const NON_PRODUCT_KINDS: [&str; 4] = ["test", "bench", "example", "custom-build"];

/// What [`discover`] found: the product sources of the whole workspace, plus
/// any advisory line the module-graph walk produced.
#[derive(Debug)]
pub(crate) struct Discovery {
    /// Every product source file, deduplicated across the workspace.
    pub(crate) sources: Vec<SourceFile>,
    /// Advisory lines about declarations the walk could not resolve. Advisory
    /// only: they never affect the exit code (C6). See [`resolve_module`].
    pub(crate) diagnostics: Vec<String>,
}

/// One product source file to analyse, with the identity the pipeline needs.
#[derive(Debug)]
pub(crate) struct SourceFile {
    /// Crate-qualified module path for the file (C3), e.g. `demo::foo::bar`.
    pub(crate) module: String,
    /// The file's path relative to the workspace root, forward-slashed. This is
    /// the path the file is *known by*: it is what the coverage join queries
    /// and what the identity fields (C15) carry.
    pub(crate) path: String,
    /// The path to actually read from, as cargo reported it.
    pub(crate) absolute: PathBuf,
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
/// `path`.
///
/// `path` only says *where* the workspace is: cargo searches it and its
/// ancestors for the manifest, exactly as it does for any cargo command, and
/// the whole workspace is then enumerated. Ordering is deterministic
/// (FC-T6b) — members by name, targets library-first then by name, files by
/// path.
pub(crate) fn discover(path: &Path) -> anyhow::Result<Discovery> {
    let dir = if path.is_dir() {
        path.to_path_buf()
    } else if path.is_file() {
        path.parent().unwrap_or(Path::new(".")).to_path_buf()
    } else {
        bail!("path not found: {}", path.display());
    };

    let metadata = parse_metadata(&cargo_metadata(&dir)?)?;
    source_files(&metadata)
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

/// Enumerate the product sources of every target in the workspace, in a
/// deterministic order: members by name, targets library-first then by name,
/// each target's files by path (FC-T6b).
///
/// **Dedup spans the whole workspace.** Cargo lets a target's `path` point
/// anywhere, so two *packages* can name the same file, and one file can also be
/// reached from a package's lib and its bin. Whichever target reaches it first
/// in the order above owns it — and so names it — and every later claim is
/// dropped, so no file is ever analysed (or reported) twice.
///
/// **Diagnostics are owned exactly as sources are.** Each file's advisory lines
/// travel *with* that file ([`FileEntry`]), so a claim discarded by dedup takes
/// its diagnostics with it and the emitted set always describes the module
/// graph the report was actually measured from — no declaration warns once per
/// target that happens to reach it.
///
/// **And that ownership is the whole of it: there is no dedup of the lines
/// themselves.** Each kept file is a distinct file (dedup is by canonical
/// identity) whose diagnostics are produced once, and within one file every
/// diagnostic names a distinct declaration site — so no condition can be
/// emitted twice. Collapsing equal *text* would not be a stronger guard but a
/// wrong one: two mutually-exclusive declarations of one module (`#[cfg(unix)]`
/// and `#[cfg(windows)]`) are two conditions, and one of them would be lost.
fn source_files(metadata: &Metadata) -> anyhow::Result<Discovery> {
    let mut packages: Vec<&Package> = metadata.packages.iter().collect();
    packages.sort_by(|a, b| a.name.cmp(&b.name));

    let mut discovery = Discovery {
        sources: Vec::new(),
        diagnostics: Vec::new(),
    };
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    for package in packages {
        let mut targets: Vec<&Target> = package.targets.iter().filter(|t| t.is_product()).collect();
        targets.sort_by(|a, b| a.order_key().cmp(&b.order_key()));

        for target in targets {
            for entry in target_sources(target, &metadata.workspace_root)? {
                if !seen.insert(entry.identity) {
                    continue;
                }
                discovery.diagnostics.extend(entry.diagnostics);
                discovery.sources.push(SourceFile {
                    module: entry.module,
                    path: workspace_relative(&metadata.workspace_root, &entry.file),
                    absolute: entry.file,
                });
            }
        }
    }
    Ok(discovery)
}

/// The identity two lexically different paths are compared by, so the same file
/// named two ways is analysed once — and, in [`target_sources`], so a module
/// graph that keeps renaming one file is walked once rather than forever.
///
/// Canonicalization is the *only* domain used: a lexical fallback would put two
/// spellings of one file in two different identity domains, and the file would
/// then be measured — and reported — twice, or walked without end. A wrong
/// number is worse than a missing one, so a file whose identity cannot be
/// established is not assumed distinct: it is a file cargo compiles that we
/// cannot resolve, which [`target_sources`] treats exactly as it treats one it
/// cannot read (C6 ⇒ exit 1). The key is never shown to anyone: on Windows
/// canonicalization yields a `\\?\` verbatim path, which is exactly why the
/// *reported* path is derived from the original.
fn identity(file: &Path) -> std::io::Result<PathBuf> {
    fs::canonicalize(file)
}

/// One file of a target's module graph, and everything rustc needs to resolve
/// the modules *that file* declares.
struct Scope {
    /// The module path from the crate root — `["demo", "foo"]` for
    /// `demo::foo`. This, not the file's location, is the module's name.
    segments: Vec<String>,
    /// The directory declarations in this file resolve against (rustc's
    /// `dir_path`).
    dir: PathBuf,
    /// The extra directory segment a non-`mod.rs` file module contributes
    /// (rustc's `DirOwnership::Owned { relative }`): `src/foo.rs` has `dir =
    /// src` and `relative = foo`, so its `mod bar;` is `src/foo/bar.rs` while a
    /// `#[path]` on that same declaration is relative to `src`.
    relative: Option<String>,
}

impl Scope {
    /// The directory an out-of-line `mod foo;` in this scope resolves in.
    fn module_dir(&self) -> PathBuf {
        match &self.relative {
            Some(segment) => self.dir.join(segment),
            None => self.dir.clone(),
        }
    }
}

/// One file of a target's module graph: what it is called, where it is, the
/// identity it is deduplicated by, and the advisory lines *its own*
/// declarations produced. Diagnostics ride with the file so that dropping a
/// duplicate claim drops its diagnostics too.
struct FileEntry {
    module: String,
    file: PathBuf,
    /// The canonical identity of `file` ([`identity`]), established once here
    /// and reused by [`source_files`] rather than recomputed — one filesystem
    /// answer, so the walk and the workspace-wide dedup cannot disagree.
    identity: PathBuf,
    diagnostics: Vec<String>,
}

/// Every source file of one target, found by walking its module graph from the
/// crate root exactly as rustc resolves it, each paired with its
/// crate-qualified module path (C3).
///
/// The crate root is the target's `src_path`, whatever it is called, and it
/// *is* the crate: `src/lib.rs`, `src/main.rs`, `src/bin/tool.rs` and an
/// explicit `[[bin]] path = "cmd/tool.rs"` all name their target. From there
/// only declared modules are reached — an inline `mod foo { .. }` stays in the
/// same file and contributes a name segment, an out-of-line `mod foo;` pulls in
/// a file (see [`resolve_module`]) — so a `.rs` file no crate root reaches is
/// not product source and is never analysed.
///
/// A file is parsed here to find its declarations and parsed again downstream
/// to measure it; the AST is dropped in between rather than held for a whole
/// workspace at once.
fn target_sources(target: &Target, workspace_root: &Path) -> anyhow::Result<Vec<FileEntry>> {
    let mut queue = VecDeque::from([(
        target.src_path.clone(),
        Scope {
            segments: vec![target.crate_ident()],
            dir: parent_of(&target.src_path),
            relative: None,
        },
    )]);

    // A `#[path]` attribute can point a module at a file already in the graph.
    // The guard is keyed on the file's *identity*, not on how the attribute
    // spelled it: a self-reference like `#[path = "../src/lib.rs"]` names one
    // file with a spelling that grows a segment every time round, so a
    // spelling-keyed guard never fires and the walk never ends.
    let mut visited: BTreeSet<PathBuf> = BTreeSet::new();
    let mut found = Vec::new();
    while let Some((file, scope)) = queue.pop_front() {
        // Every queued file was reported by cargo or found on disk by
        // `resolve_module`. One we cannot resolve now is one we cannot read
        // either — a compiled file missing from the report, i.e. the same
        // operational error `parse_source` raises (C6 ⇒ exit 1) — and guessing
        // an identity for it would put the walk back on the unbounded path.
        let identity = identity(&file)
            .with_context(|| format!("failed to resolve source file {}", file.display()))?;
        if !visited.insert(identity.clone()) {
            continue;
        }
        let ast = parse_source(&file)?;
        let mut diagnostics = Vec::new();
        declared_modules(
            &ast.items,
            &scope,
            &file,
            workspace_root,
            &mut diagnostics,
            &mut queue,
        );
        found.push(FileEntry {
            module: scope.segments.join("::"),
            file,
            identity,
            diagnostics,
        });
    }
    found.sort_by(|a, b| a.file.cmp(&b.file));
    Ok(found)
}

/// Queue every out-of-line module `items` declares, descending through inline
/// `mod foo { .. }` blocks — which contribute a name *and* a directory segment
/// without being a file of their own.
///
/// Every advisory line a declaration produces is prefixed with that
/// declaration's `file:line:column`, the way rustc points at one. Two
/// declarations of the same module are two conditions — `#[cfg(unix)] mod imp;`
/// and `#[cfg(windows)] mod imp;` are both real — so a line that named only the
/// file and the module could not tell them apart, and a reader could not tell
/// *which* declaration went unresolved.
fn declared_modules(
    items: &[syn::Item],
    scope: &Scope,
    file: &Path,
    workspace_root: &Path,
    diagnostics: &mut Vec<String>,
    queue: &mut VecDeque<(PathBuf, Scope)>,
) {
    for item in items {
        let syn::Item::Mod(declaration) = item else {
            continue;
        };
        let name = declaration.ident.to_string();
        let mut segments = scope.segments.clone();
        segments.push(name.clone());
        let start = declaration.ident.span().start();
        let site = format!(
            "{}:{}:{}",
            workspace_relative(workspace_root, file),
            start.line,
            // proc-macro2 columns are 0-based; rustc's are 1-based.
            start.column + 1,
        );

        let attr_path = match module_path(&declaration.attrs) {
            ModulePath::Default => None,
            ModulePath::Fixed(path) => Some(path),
            ModulePath::Conditional(attribute) => {
                diagnostics.push(format!(
                    "warning: {site}: module `{}` selects its source file conditionally \
                     (`{attribute}`); which file rustc compiles depends on the cfg set, \
                     which is not evaluated, so it is not analysed",
                    segments.join("::"),
                ));
                continue;
            }
        };

        if let Some((_, inline)) = &declaration.content {
            // `#[path]` on an inline module names the directory its own
            // declarations resolve in, relative to this file's directory.
            let dir = match &attr_path {
                Some(path) => scope.dir.join(path),
                None => scope.module_dir().join(&name),
            };
            let inner = Scope {
                segments,
                dir,
                relative: None,
            };
            declared_modules(inline, &inner, file, workspace_root, diagnostics, queue);
            continue;
        }

        if let Some(child) = resolve_module(
            &name,
            attr_path.as_deref(),
            scope,
            &segments,
            &site,
            workspace_root,
            diagnostics,
        ) {
            queue.push_back(child);
        }
    }
}

/// Resolve one out-of-line `mod name;` to the file it loads, per rustc:
/// `<dir>/name.rs` or `<dir>/name/mod.rs`, or the `#[path]` the declaration
/// gives (relative to the declaring file's directory). `#[path]` is honoured
/// because rustc honours it: ignoring it drops a compiled file, or names one
/// after where it happens to live instead of what it is.
///
/// **A module that resolves to nothing is a diagnostic, not an error.** Such a
/// workspace does not compile as-is, but the same declaration is legitimately
/// unresolvable for us when it is behind an inactive `cfg` whose file was never
/// written — and we do not evaluate `cfg` (T10). A reporter that refused to
/// report at all would be worse than one that reports what it found and says
/// what it could not reach; what it must never do is stay silent (C6: advisory,
/// exit code unaffected).
fn resolve_module(
    name: &str,
    attr_path: Option<&str>,
    scope: &Scope,
    segments: &[String],
    site: &str,
    workspace_root: &Path,
    diagnostics: &mut Vec<String>,
) -> Option<(PathBuf, Scope)> {
    let module = segments.join("::");
    let dir = scope.module_dir();
    let candidates: Vec<PathBuf> = match attr_path {
        Some(path) => vec![scope.dir.join(path)],
        None => vec![
            dir.join(format!("{name}.rs")),
            dir.join(name).join("mod.rs"),
        ],
    };

    let existing: Vec<&PathBuf> = candidates.iter().filter(|c| c.is_file()).collect();
    let Some(&child) = existing.first() else {
        diagnostics.push(format!(
            "warning: {site}: module `{module}` is declared but no file was found for it \
             (looked for {}); it is not analysed",
            candidates
                .iter()
                .map(|c| workspace_relative(workspace_root, c))
                .collect::<Vec<_>>()
                .join(" and "),
        ));
        return None;
    };
    if existing.len() > 1 {
        diagnostics.push(format!(
            "warning: {site}: module `{module}` has files at both {}; {} is analysed",
            existing
                .iter()
                .map(|c| workspace_relative(workspace_root, c))
                .collect::<Vec<_>>()
                .join(" and "),
            workspace_relative(workspace_root, child),
        ));
    }

    // What the loaded file's own declarations resolve against: a `#[path]` file
    // owns its directory outright, `foo/mod.rs` owns `foo/`, and `foo.rs` owns
    // `foo/` only once its own declarations are reached (`relative`).
    let (dir, relative) = if attr_path.is_some() {
        (parent_of(child), None)
    } else if child.file_name().is_some_and(|n| n == "mod.rs") {
        (dir.join(name), None)
    } else {
        (dir, Some(name.to_string()))
    };
    Some((
        child.clone(),
        Scope {
            segments: segments.to_vec(),
            dir,
            relative,
        },
    ))
}

/// Where a `mod` declaration's source file comes from, as far as we can tell
/// without evaluating `cfg`.
enum ModulePath {
    /// No `path` attribute: rustc's default filename rules apply.
    Default,
    /// A direct `#[path = "..."]` — unconditional, so we know the file.
    Fixed(String),
    /// A `cfg_attr` whose expansion could supply a `path` — held as its tokens
    /// read, so the diagnostic can name the predicate that went unevaluated:
    /// *which* file rustc compiles depends on the cfg set, which we do not
    /// evaluate.
    Conditional(String),
}

/// Classify a `mod` declaration's attributes into [`ModulePath`].
///
/// A conditional `path` outranks a direct one: if both are present, the active
/// cfg set decides which applies (and rustc rejects two active ones), so we
/// still do not know the file.
fn module_path(attrs: &[syn::Attribute]) -> ModulePath {
    let conditional: Vec<String> = attrs
        .iter()
        .filter_map(conditional_path_attribute)
        .collect();
    if !conditional.is_empty() {
        return ModulePath::Conditional(conditional.join(" "));
    }
    match path_attribute(attrs) {
        Some(path) => ModulePath::Fixed(path),
        None => ModulePath::Default,
    }
}

/// The attribute if it is a `cfg_attr` whose expansion could supply a `path`,
/// rendered from its tokens (`#[cfg_attr(unix , path = "unix.rs")]`) — token
/// spacing, not the source spelling, but every particular the declaration gave.
fn conditional_path_attribute(attr: &syn::Attribute) -> Option<String> {
    if !attr.path().is_ident("cfg_attr") || !supplies_path(&attr.meta) {
        return None;
    }
    let syn::Meta::List(list) = &attr.meta else {
        return None;
    };
    Some(format!("#[cfg_attr({})]", list.tokens))
}

/// `true` if this `cfg_attr` meta expands to (or nests another `cfg_attr` that
/// expands to) a `path` attribute.
fn supplies_path(meta: &syn::Meta) -> bool {
    let syn::Meta::List(list) = meta else {
        return false;
    };
    match list.parse_args_with(Punctuated::<syn::Meta, Token![,]>::parse_terminated) {
        // The first element is the cfg predicate; everything after it is an
        // attribute the expansion applies.
        Ok(applied) => applied.iter().skip(1).any(|applied| {
            applied.path().is_ident("path")
                || (applied.path().is_ident("cfg_attr") && supplies_path(applied))
        }),
        // Arguments we cannot read might well name a `path`. Declining to
        // answer for one module is cheap; guessing its source file is not.
        Err(_) => true,
    }
}

/// The string of a direct `#[path = "..."]` attribute, if the declaration has
/// one.
fn path_attribute(attrs: &[syn::Attribute]) -> Option<String> {
    attrs.iter().find_map(|attr| {
        if !attr.path().is_ident("path") {
            return None;
        }
        let syn::Meta::NameValue(name_value) = &attr.meta else {
            return None;
        };
        match &name_value.value {
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(literal),
                ..
            }) => Some(literal.value()),
            _ => None,
        }
    })
}

/// Read and parse one source file. A crate root or a declared module that
/// cannot be read or parsed is an operational error (C6 ⇒ exit 1): it is a file
/// cargo compiles, so the report would be missing part of the product.
fn parse_source(file: &Path) -> anyhow::Result<syn::File> {
    let src = fs::read_to_string(file)
        .with_context(|| format!("failed to read source file {}", file.display()))?;
    syn::parse_file(&src).with_context(|| format!("failed to parse Rust source {}", file.display()))
}

/// The directory containing `file`, falling back to the current directory for a
/// bare filename.
fn parent_of(file: &Path) -> PathBuf {
    file.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .to_path_buf()
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
fn workspace_relative(workspace_root: &Path, file: &Path) -> String {
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
        assert!(
            err.to_string().contains("cargo metadata"),
            "{}",
            err.to_string()
        );
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

    /// A throwaway on-disk source tree. Module resolution *is* "which of these
    /// files exists", so the graph walk is exercised against real files in an
    /// isolated per-test directory rather than against a mock.
    struct Tree(PathBuf);

    impl Tree {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("crap4rust-graph-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("create tree root");
            Self(dir)
        }

        fn write(&self, relative: &str, contents: &str) -> &Self {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().expect("a parent directory")).expect("create dir");
            fs::write(&path, contents).expect("write source");
            self
        }

        /// Walk the graph of a target named `name` rooted at `root`, yielding
        /// `(module path, tree-relative file)` pairs plus any diagnostic.
        fn walk(&self, name: &str, root: &str) -> (Vec<(String, String)>, Vec<String>) {
            let root = self.0.join(root);
            let target = target(name, "lib", &root.to_string_lossy());
            let found = target_sources(&target, &self.0).expect("walk the graph");
            let mut diagnostics = Vec::new();
            let mut files = Vec::new();
            for entry in found {
                diagnostics.extend(entry.diagnostics);
                files.push((entry.module, workspace_relative(&self.0, &entry.file)));
            }
            (files, diagnostics)
        }

        /// Enumerate the whole "workspace" the way [`discover`] does — through
        /// dedup and diagnostic ownership — from targets given as
        /// `(package, target, root)`.
        fn enumerate(&self, targets: &[(&str, &str, &str)]) -> Discovery {
            let packages = targets
                .iter()
                .map(|(package, name, root)| Package {
                    name: package.to_string(),
                    targets: vec![target(name, "lib", &self.0.join(root).to_string_lossy())],
                })
                .collect();
            source_files(&Metadata {
                workspace_root: self.0.clone(),
                packages,
            })
            .expect("enumerate the workspace")
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn modules(files: &[(String, String)]) -> Vec<&str> {
        files.iter().map(|(module, _)| module.as_str()).collect()
    }

    /// Defect 1, by construction: a file no crate root declares is not compiled
    /// by cargo, so it is not product source — however inviting the directory
    /// it sits in looks.
    #[test]
    fn only_modules_reachable_from_the_crate_root_are_enumerated() {
        let tree = Tree::new("orphan");
        tree.write("src/lib.rs", "mod foo;\n")
            .write("src/foo.rs", "")
            .write("src/orphan.rs", "")
            .write("src/nested/orphan.rs", "");

        let (files, diagnostics) = tree.walk("demo", "src/lib.rs");

        assert_eq!(
            files,
            vec![
                ("demo::foo".to_string(), "src/foo.rs".to_string()),
                ("demo".to_string(), "src/lib.rs".to_string()),
            ]
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    /// The crate root file *is* the crate, and below it the graph — not the
    /// path — names each module: `mod.rs` names its parent, and a file module's
    /// own submodules live under its stem.
    #[test]
    fn file_and_directory_modules_are_named_by_the_graph() {
        let tree = Tree::new("nesting");
        tree.write("src/lib.rs", "mod foo;\n")
            .write("src/foo/mod.rs", "mod bar;\n")
            .write("src/foo/bar.rs", "mod baz;\n")
            .write("src/foo/bar/baz.rs", "");

        let (files, diagnostics) = tree.walk("demo", "src/lib.rs");

        assert_eq!(
            files,
            vec![
                (
                    "demo::foo::bar::baz".to_string(),
                    "src/foo/bar/baz.rs".to_string()
                ),
                ("demo::foo::bar".to_string(), "src/foo/bar.rs".to_string()),
                ("demo::foo".to_string(), "src/foo/mod.rs".to_string()),
                ("demo".to_string(), "src/lib.rs".to_string()),
            ]
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    /// An inline `mod` is not a file, but it contributes both a name segment
    /// and a directory segment to the modules it declares.
    #[test]
    fn an_inline_module_contributes_a_name_and_a_directory() {
        let tree = Tree::new("inline");
        tree.write("src/lib.rs", "mod outer {\n    mod inner;\n}\n")
            .write("src/outer/inner.rs", "");

        let (files, _) = tree.walk("demo", "src/lib.rs");

        assert_eq!(
            files,
            vec![
                ("demo".to_string(), "src/lib.rs".to_string()),
                (
                    "demo::outer::inner".to_string(),
                    "src/outer/inner.rs".to_string()
                ),
            ]
        );
    }

    /// `#[path]` decouples a module's file from its name — which is precisely
    /// why path-derived naming is wrong and the graph is authoritative. The
    /// relocated file's *own* declarations resolve in its new directory.
    #[test]
    fn a_path_attribute_relocates_the_file_without_renaming_the_module() {
        let tree = Tree::new("path_attr");
        tree.write(
            "src/lib.rs",
            "#[path = \"elsewhere/renamed.rs\"]\nmod aliased;\n",
        )
        .write("src/elsewhere/renamed.rs", "mod child;\n")
        .write("src/elsewhere/child.rs", "");

        let (files, diagnostics) = tree.walk("demo", "src/lib.rs");

        assert_eq!(
            files,
            vec![
                (
                    "demo::aliased::child".to_string(),
                    "src/elsewhere/child.rs".to_string()
                ),
                (
                    "demo::aliased".to_string(),
                    "src/elsewhere/renamed.rs".to_string()
                ),
                ("demo".to_string(), "src/lib.rs".to_string()),
            ]
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    /// Defect 2: a crate root that is not `lib.rs`/`main.rs` owns its own
    /// directory exactly as rustc says, so a sibling module of `cmd/tool.rs` is
    /// compiled product source and must be enumerated — not quietly dropped
    /// because the directory is shared.
    #[test]
    fn a_nonstandard_crate_root_reaches_its_sibling_modules() {
        let tree = Tree::new("nonstandard_root");
        tree.write("cmd/tool.rs", "mod helper;\n")
            .write("cmd/helper.rs", "")
            .write("cmd/other.rs", "");

        let (files, _) = tree.walk("tool", "cmd/tool.rs");

        assert_eq!(modules(&files), vec!["tool::helper", "tool"], "{files:?}");
        // `cmd/other.rs` is another bin's root, or nothing at all — either way
        // this crate does not declare it, so it is not this crate's source.
        assert!(
            !files.iter().any(|(_, file)| file == "cmd/other.rs"),
            "{files:?}"
        );
    }

    /// A declared module with no file is a real condition, not something to
    /// pass over in silence: it is advisory (C6), it names both places rustc
    /// would have looked, and the rest of the graph is still enumerated.
    #[test]
    fn a_declared_module_with_no_file_is_diagnosed() {
        let tree = Tree::new("missing_module");
        tree.write("src/lib.rs", "mod present;\nmod absent;\n")
            .write("src/present.rs", "");

        let (files, diagnostics) = tree.walk("demo", "src/lib.rs");

        assert_eq!(modules(&files), vec!["demo", "demo::present"], "{files:?}");
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert!(diagnostics[0].contains("demo::absent"), "{diagnostics:?}");
        assert!(diagnostics[0].contains("src/absent.rs"), "{diagnostics:?}");
        assert!(
            diagnostics[0].contains("src/absent/mod.rs"),
            "{diagnostics:?}"
        );
    }

    /// Both files present is an error to rustc; here it is a warning naming
    /// both, so which one the numbers describe is never a guess.
    #[test]
    fn a_module_with_two_candidate_files_is_diagnosed() {
        let tree = Tree::new("ambiguous_module");
        tree.write("src/lib.rs", "mod foo;\n")
            .write("src/foo.rs", "")
            .write("src/foo/mod.rs", "");

        let (files, diagnostics) = tree.walk("demo", "src/lib.rs");

        assert_eq!(modules(&files), vec!["demo::foo", "demo"], "{files:?}");
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert!(diagnostics[0].contains("src/foo.rs"), "{diagnostics:?}");
        assert!(diagnostics[0].contains("src/foo/mod.rs"), "{diagnostics:?}");
    }

    /// The T10 seam: *loading* a declared module is graph resolution and
    /// happens here, whatever attributes guard the declaration. Deciding that
    /// what it contains is test code — and dropping it — is T10 filtering.
    #[test]
    fn a_cfg_guarded_module_declaration_is_still_resolved() {
        let tree = Tree::new("cfg_module");
        tree.write("src/lib.rs", "#[cfg(test)]\nmod tests;\n")
            .write("src/tests.rs", "");

        let (files, diagnostics) = tree.walk("demo", "src/lib.rs");

        assert_eq!(modules(&files), vec!["demo", "demo::tests"], "{files:?}");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn target_name_hyphens_become_crate_underscores() {
        let tree = Tree::new("hyphens");
        tree.write("src/lib.rs", "mod foo;\n")
            .write("src/foo.rs", "");

        let (files, _) = tree.walk("my-crate", "src/lib.rs");

        assert_eq!(modules(&files), vec!["my_crate::foo", "my_crate"]);
    }

    /// Defect 3: dedup compares files, not spellings. Two spellings of one file
    /// must land in **one** identity domain, or the file is measured twice and
    /// the report silently reports a bigger product than exists.
    #[test]
    fn two_spellings_of_one_file_are_enumerated_once() {
        let tree = Tree::new("dedup");
        tree.write("src/lib.rs", "");

        let discovery = tree.enumerate(&[
            ("alpha", "alpha", "src/lib.rs"),
            ("beta", "beta", "src/../src/lib.rs"),
        ]);

        let modules: Vec<&str> = discovery
            .sources
            .iter()
            .map(|s| s.module.as_str())
            .collect();
        assert_eq!(modules, vec!["alpha"], "{:?}", discovery.sources);
        assert!(discovery.diagnostics.is_empty(), "{discovery:?}");
        assert_eq!(
            identity(&tree.0.join("src").join("lib.rs")).expect("identity"),
            identity(&tree.0.join("src").join("..").join("src").join("lib.rs")).expect("identity")
        );
        // Where identity cannot be established there is no second domain to
        // fall back to; the walk raises it rather than assuming distinctness.
        assert!(identity(&tree.0.join("src").join("absent.rs")).is_err());
    }

    /// Defect 2: diagnostics carry the same ownership as sources. One file
    /// reached from two targets declares one missing module, so exactly one
    /// warning is emitted — not one per claimant, and none at all from the
    /// claim dedup discarded.
    #[test]
    fn a_file_reached_from_two_targets_warns_once() {
        let tree = Tree::new("shared_claim");
        tree.write("src/lib.rs", "mod absent;\n");

        let discovery = tree.enumerate(&[
            ("alpha", "alpha", "src/lib.rs"),
            ("beta", "beta", "src/lib.rs"),
        ]);

        assert_eq!(discovery.sources.len(), 1, "{:?}", discovery.sources);
        assert_eq!(discovery.diagnostics.len(), 1, "{discovery:?}");
        assert!(
            discovery.diagnostics[0].contains("alpha::absent"),
            "{discovery:?}"
        );
    }

    /// Defect 1: `#[cfg_attr(.., path = "..")]` chooses which file rustc
    /// compiles. With the default file present too, silently analysing it would
    /// produce a confident report of source rustc may never have compiled — so
    /// the module is diagnosed and left unanalysed instead.
    #[test]
    fn a_conditionally_pathed_module_is_diagnosed_not_guessed() {
        let tree = Tree::new("cfg_attr_path");
        tree.write(
            "src/lib.rs",
            "#[cfg_attr(windows, path = \"windows.rs\")]\nmod imp;\n",
        )
        .write("src/imp.rs", "")
        .write("src/windows.rs", "");

        let (files, diagnostics) = tree.walk("demo", "src/lib.rs");

        assert_eq!(modules(&files), vec!["demo"], "{files:?}");
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert!(diagnostics[0].contains("demo::imp"), "{diagnostics:?}");
        assert!(diagnostics[0].contains("cfg_attr"), "{diagnostics:?}");
        // The line names the declaration (file:line:column) and the predicate
        // that was not evaluated — enough for a reader to go and look.
        assert!(diagnostics[0].contains("src/lib.rs:2:5"), "{diagnostics:?}");
        assert!(
            diagnostics[0].contains("path = \"windows.rs\""),
            "{diagnostics:?}"
        );
    }

    /// Two mutually-exclusive declarations of one module are two conditions,
    /// however alike they read: collapsing them would silently drop one, and a
    /// reader would never learn that the other exists.
    #[test]
    fn mutually_exclusive_declarations_of_one_module_stay_distinct() {
        let tree = Tree::new("exclusive_declarations");
        tree.write(
            "src/lib.rs",
            "#[cfg(unix)]\n#[cfg_attr(unix, path = \"unix.rs\")]\nmod imp;\n\
             #[cfg(windows)]\n#[cfg_attr(windows, path = \"windows.rs\")]\nmod imp;\n",
        );

        let discovery = tree.enumerate(&[("demo", "demo", "src/lib.rs")]);

        assert_eq!(discovery.diagnostics.len(), 2, "{discovery:?}");
        assert!(
            discovery.diagnostics[0].contains("src/lib.rs:3:5")
                && discovery.diagnostics[0].contains("path = \"unix.rs\""),
            "{discovery:?}"
        );
        assert!(
            discovery.diagnostics[1].contains("src/lib.rs:6:5")
                && discovery.diagnostics[1].contains("path = \"windows.rs\""),
            "{discovery:?}"
        );
    }

    /// A `#[path]` that names the file it is written in spells that one file a
    /// new way every time round (`src/../src/../src/lib.rs`), so a guard keyed
    /// on the spelling never fires and the walk never returns. Identity is what
    /// makes it finite. The timeout keeps a regression *failing* rather than
    /// wedging the suite.
    #[test]
    fn a_self_referential_path_attribute_terminates() {
        let tree = Tree::new("path_cycle");
        tree.write("src/lib.rs", "#[path = \"../src/lib.rs\"]\nmod again;\n");

        let (root, workspace_root) = (tree.0.join("src/lib.rs"), tree.0.clone());
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let target = target("demo", "lib", &root.to_string_lossy());
            let _ = sender.send(target_sources(&target, &workspace_root).map(|found| found.len()));
        });

        match receiver.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(walked) => assert_eq!(walked.expect("walk the graph"), 1),
            Err(_) => panic!("the module-graph walk did not terminate"),
        }
    }

    /// The same holds for an inline module: a conditional `path` there names
    /// the directory its children resolve in, so descending would look for them
    /// in a directory rustc may not use.
    #[test]
    fn a_conditionally_pathed_inline_module_is_not_descended() {
        let tree = Tree::new("cfg_attr_inline");
        tree.write(
            "src/lib.rs",
            "#[cfg_attr(unix, path = \"unix\")]\nmod outer {\n    mod inner;\n}\n",
        )
        .write("src/outer/inner.rs", "");

        let (files, diagnostics) = tree.walk("demo", "src/lib.rs");

        assert_eq!(modules(&files), vec!["demo"], "{files:?}");
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert!(diagnostics[0].contains("demo::outer"), "{diagnostics:?}");
    }

    /// Only a `cfg_attr` that could actually supply a `path` withdraws a
    /// module; the ordinary ones must not cost us coverage of the graph.
    #[test]
    fn cfg_attr_without_a_path_leaves_resolution_alone() {
        let attrs = |source: &str| {
            syn::parse_file(source)
                .expect("parse")
                .items
                .into_iter()
                .find_map(|item| match item {
                    syn::Item::Mod(declaration) => Some(declaration.attrs),
                    _ => None,
                })
                .expect("a mod declaration")
        };

        assert!(matches!(
            module_path(&attrs("#[cfg_attr(test, allow(dead_code))] mod foo;")),
            ModulePath::Default
        ));
        assert!(matches!(
            module_path(&attrs("#[cfg(unix)] mod foo;")),
            ModulePath::Default
        ));
        assert!(matches!(
            module_path(&attrs("#[path = \"a.rs\"] mod foo;")),
            ModulePath::Fixed(path) if path == "a.rs"
        ));
        assert!(matches!(
            module_path(&attrs(
                "#[cfg_attr(all(unix, feature = \"x\"), path = \"a.rs\")] mod foo;"
            )),
            ModulePath::Conditional(attribute)
                if attribute.starts_with("#[cfg_attr(") && attribute.contains("path = \"a.rs\"")
        ));
        // Nested, and alongside a direct `#[path]`: the cfg set still decides.
        assert!(matches!(
            module_path(&attrs(
                "#[cfg_attr(unix, cfg_attr(target_os = \"macos\", path = \"a.rs\"))] mod foo;"
            )),
            ModulePath::Conditional(_)
        ));
        assert!(matches!(
            module_path(&attrs(
                "#[path = \"a.rs\"]\n#[cfg_attr(windows, path = \"b.rs\")] mod foo;"
            )),
            ModulePath::Conditional(_)
        ));
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
        let err = discover(Path::new("no/such/directory")).unwrap_err();
        assert!(err.to_string().contains("path not found"));
    }
}
