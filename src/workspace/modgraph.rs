//! Module-graph resolution (FC-T9k) — rustc's rules, not cargo's.
//!
//! [`super`] answers "which crates does cargo build, and where is each one
//! rooted"; this module answers "which files does rustc compile for one crate
//! root, and what is each of them called". They change for different reasons —
//! cargo's manifest schema vs rustc's module-resolution rules — so they are
//! separate files.
//!
//! [`Scope`] models rustc's `DirOwnership::Owned { relative }`: a crate root
//! and a `mod.rs` own their directory, a file module `foo.rs` owns `foo/` only
//! for its own declarations. Names come from the graph
//! (`scope.segments.join("::")`), **never** from the filesystem — `#[path]`
//! decouples a file's location from its module path, so the graph is the truth.
//!
//! **One parse per file (FC-T9h).** The walk parses each file to find its
//! declarations and hands that very AST to the consumer ([`Walker::walk`]'s
//! `visit`), one file at a time: peak memory is a single `syn::File`, and there
//! is no second parse anywhere in the pipeline —
//! [`crate::complexity::analyze_str`] is compiled out of production builds.
//! Enumeration stays sequential and order-dependent (FC-T9i): the first
//! claimant of a file owns and names it, which is what makes ordering
//! deterministic (FC-T6b).
//!
//! **One `cfg` construct is not deferrable.** `#[cfg(..)]` decides *whether* a
//! module is compiled; `#[cfg_attr(.., path = "..")]` decides *which file* it
//! is compiled from. Guessing the former over-reports at worst; guessing the
//! latter measures a file rustc never compiled and reports the numbers as if it
//! had. So a conditionally-pathed module is diagnosed and left unanalysed — we
//! may decline to answer, but never answer confidently and wrongly. Advisory
//! only: the exit code is unaffected (C6), though the report itself says how
//! much was declined (C18).

use std::collections::{BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use syn::punctuated::Punctuated;
use syn::Token;

use super::workspace_relative;
use crate::diagnostic::{Diagnostic, Kind, Site};

/// One file of the module graph, streamed to the consumer as it is walked.
pub(super) struct Module<'a> {
    /// Crate-qualified module path of the file (C3), e.g. `demo::foo::bar`.
    pub(super) module: String,
    /// The file itself, as cargo reported it or as the graph resolved it.
    pub(super) file: &'a Path,
    /// The AST the walk parsed to find this file's declarations — handed on so
    /// nothing parses it a second time (FC-T9h).
    pub(super) ast: &'a syn::File,
}

/// Walks module graphs, streaming each file's AST to a consumer.
///
/// One walker spans the whole workspace so that the file-identity guard is
/// shared: a file reachable from two targets is walked — and therefore named,
/// measured and diagnosed — exactly once, by whichever target reaches it first.
pub(super) struct Walker<'a> {
    workspace_root: &'a Path,
    diagnostics: &'a mut Vec<Diagnostic>,
    /// Canonical identities of every file already walked. Keyed on identity,
    /// not on spelling: a self-reference like `#[path = "../src/lib.rs"]` names
    /// one file with a spelling that grows a segment every time round, so a
    /// spelling-keyed guard never fires and the walk never ends. It is also the
    /// workspace-wide dedup domain: two spellings of one file must never be two
    /// claims, or the file is measured — and reported — twice.
    visited: BTreeSet<PathBuf>,
    /// How many files this walker has parsed — the observable half of "one
    /// parse per file" (FC-T9h).
    #[cfg(test)]
    parses: usize,
}

impl<'a> Walker<'a> {
    pub(super) fn new(workspace_root: &'a Path, diagnostics: &'a mut Vec<Diagnostic>) -> Self {
        Self {
            workspace_root,
            diagnostics,
            visited: BTreeSet::new(),
            #[cfg(test)]
            parses: 0,
        }
    }

    /// Walk one crate's module graph from `root`, calling `visit` once per file
    /// with the AST just parsed for it.
    ///
    /// The crate root *is* the crate, whatever it is called: `src/lib.rs`,
    /// `src/main.rs`, `src/bin/tool.rs` and an explicit `[[bin]] path =
    /// "cmd/tool.rs"` all name their target. From there only declared modules
    /// are reached — an inline `mod foo { .. }` stays in the same file and
    /// contributes a name segment, an out-of-line `mod foo;` pulls in a file
    /// (see [`resolve_module`]) — so a `.rs` file no crate root reaches is not
    /// product source and is never analysed.
    ///
    /// Files are visited in graph order: the root, then the modules it
    /// declares, breadth-first, each file's declarations in source order.
    pub(super) fn walk(
        &mut self,
        root: &Path,
        crate_ident: &str,
        visit: &mut dyn FnMut(Module<'_>) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let mut queue = VecDeque::from([(
            root.to_path_buf(),
            Scope {
                segments: vec![crate_ident.to_string()],
                dir: parent_of(root),
                relative: None,
            },
        )]);

        while let Some((file, scope)) = queue.pop_front() {
            // Every queued file was reported by cargo or found on disk by
            // `resolve_module`. One we cannot resolve now is one we cannot read
            // either — a compiled file missing from the report, i.e. the same
            // operational error `parse_source` raises (C6 ⇒ exit 1) — and
            // guessing an identity for it would put the walk back on the
            // unbounded path.
            let identity = fs::canonicalize(&file)
                .with_context(|| format!("failed to resolve source file {}", file.display()))?;
            if !self.visited.insert(identity) {
                continue;
            }
            let ast = parse_source(&file)?;
            #[cfg(test)]
            {
                self.parses += 1;
            }
            declared_modules(
                &ast.items,
                &scope,
                &file,
                self.workspace_root,
                self.diagnostics,
                &mut queue,
            );
            visit(Module {
                module: scope.segments.join("::"),
                file: &file,
                ast: &ast,
            })?;
        }
        Ok(())
    }

    /// How many files have been parsed so far (FC-T9h).
    #[cfg(test)]
    pub(super) fn parses(&self) -> usize {
        self.parses
    }
}

/// One file's resolution context: what it is called, where its declarations
/// resolve, and the extra directory segment it contributes.
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

/// Queue every out-of-line module `items` declares, descending through inline
/// `mod foo { .. }` blocks — which contribute a name *and* a directory segment
/// without being a file of their own.
///
/// Every diagnostic a declaration produces points at that declaration's
/// `file:line:column`, the way rustc points at one. Two declarations of the
/// same module are two conditions — `#[cfg(unix)] mod imp;` and
/// `#[cfg(windows)] mod imp;` are both real — so a line that named only the
/// file and the module could not tell them apart, and a reader could not tell
/// *which* declaration went unresolved.
fn declared_modules(
    items: &[syn::Item],
    scope: &Scope,
    file: &Path,
    workspace_root: &Path,
    diagnostics: &mut Vec<Diagnostic>,
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
        let site = Site::at(
            workspace_relative(workspace_root, file),
            start.line,
            // proc-macro2 columns are 0-based; rustc's are 1-based.
            start.column + 1,
        );

        let attr_path = match module_path(&declaration.attrs) {
            ModulePath::Default => None,
            ModulePath::Fixed(path) => Some(path),
            ModulePath::Conditional(attribute) => {
                diagnostics.push(Diagnostic::run(
                    site,
                    Kind::ModulePathConditional {
                        module: segments.join("::"),
                        attribute,
                    },
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
/// **A module that resolves to nothing is diagnosed, not fatal.** Such a
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
    site: &Site,
    workspace_root: &Path,
    diagnostics: &mut Vec<Diagnostic>,
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
    let named = |paths: &mut dyn Iterator<Item = &PathBuf>| -> Vec<String> {
        paths
            .map(|c| workspace_relative(workspace_root, c))
            .collect()
    };

    let existing: Vec<&PathBuf> = candidates.iter().filter(|c| c.is_file()).collect();
    let Some(&child) = existing.first() else {
        diagnostics.push(Diagnostic::run(
            site.clone(),
            Kind::ModuleFileMissing {
                module,
                candidates: named(&mut candidates.iter()),
            },
        ));
        return None;
    };
    if existing.len() > 1 {
        diagnostics.push(Diagnostic::run(
            site.clone(),
            Kind::ModuleFileAmbiguous {
                module,
                candidates: named(&mut existing.iter().copied()),
                analysed: workspace_relative(workspace_root, child),
            },
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

/// Read and parse one source file — the **only** parse site in the pipeline
/// (FC-T9h). A crate root or a declared module that cannot be read or parsed is
/// an operational error (C6 ⇒ exit 1): it is a file cargo compiles, so the
/// report would be missing part of the product.
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

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// A throwaway on-disk source tree. Module resolution *is* "which of these
    /// files exists", so the graph walk is exercised against real files in an
    /// isolated per-test directory rather than against a mock: a mock
    /// filesystem would be less truthful, and the fidelity is the point
    /// (FC-T9k).
    pub(crate) struct Tree(pub(crate) PathBuf);

    impl Tree {
        pub(crate) fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("crap4rust-graph-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("create tree root");
            Self(dir)
        }

        pub(crate) fn write(&self, relative: &str, contents: &str) -> &Self {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().expect("a parent directory")).expect("create dir");
            fs::write(&path, contents).expect("write source");
            self
        }

        /// Walk the graph of a crate named `name` rooted at `root`, yielding
        /// `(module path, tree-relative file)` pairs plus any diagnostic.
        fn walk(&self, name: &str, root: &str) -> (Vec<(String, String)>, Vec<Diagnostic>) {
            let mut diagnostics = Vec::new();
            let mut files = Vec::new();
            Walker::new(&self.0, &mut diagnostics)
                .walk(&self.0.join(root), name, &mut |module| {
                    files.push((module.module, workspace_relative(&self.0, module.file)));
                    Ok(())
                })
                .expect("walk the graph");
            (files, diagnostics)
        }
    }

    impl Drop for Tree {
        /// Cleanup runs even when the test panics, so a failing assertion never
        /// leaves a tree behind for the next run to trip over.
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn modules(files: &[(String, String)]) -> Vec<&str> {
        files.iter().map(|(module, _)| module.as_str()).collect()
    }

    fn rendered(diagnostics: &[Diagnostic]) -> Vec<String> {
        diagnostics.iter().map(Diagnostic::to_string).collect()
    }

    /// FC-T9h: every file is parsed exactly once, and the AST the walk parsed
    /// is the one the consumer measures — the consumer never reads the file at
    /// all, so there is no second parse to count.
    #[test]
    fn each_file_is_parsed_exactly_once_and_its_ast_is_streamed() {
        let tree = Tree::new("single_parse");
        tree.write("src/lib.rs", "mod foo;\nfn root() {}\n")
            .write("src/foo.rs", "mod bar;\nfn one() {}\nfn two() {}\n")
            .write("src/foo/bar.rs", "fn three() {}\n");

        let mut diagnostics = Vec::new();
        let mut items = Vec::new();
        let mut walker = Walker::new(&tree.0, &mut diagnostics);
        walker
            .walk(&tree.0.join("src/lib.rs"), "demo", &mut |module| {
                items.push((module.module, module.ast.items.len()));
                Ok(())
            })
            .expect("walk the graph");

        // Three files, three parses, three ASTs handed over — in graph order.
        assert_eq!(walker.parses(), 3);
        assert_eq!(
            items,
            vec![
                ("demo".to_string(), 2),
                ("demo::foo".to_string(), 3),
                ("demo::foo::bar".to_string(), 1),
            ]
        );
    }

    /// A file reached from two crate roots is parsed once, not once per
    /// claimant: the identity guard spans the whole walker, and the first
    /// claimant owns and names it (FC-T9i).
    #[test]
    fn a_file_reached_from_two_targets_is_parsed_once() {
        let tree = Tree::new("shared_parse");
        tree.write("src/lib.rs", "fn one() {}\n");

        let mut diagnostics = Vec::new();
        let mut modules = Vec::new();
        let mut walker = Walker::new(&tree.0, &mut diagnostics);
        for crate_ident in ["alpha", "beta"] {
            walker
                .walk(&tree.0.join("src/lib.rs"), crate_ident, &mut |module| {
                    modules.push(module.module);
                    Ok(())
                })
                .expect("walk the graph");
        }

        assert_eq!(walker.parses(), 1);
        assert_eq!(modules, vec!["alpha".to_string()]);
    }

    /// A consumer failure stops the walk and propagates: measuring a file
    /// cargo compiles is not optional (C6 ⇒ exit 1).
    #[test]
    fn a_consumer_error_stops_the_walk() {
        let tree = Tree::new("consumer_error");
        tree.write("src/lib.rs", "mod foo;\n")
            .write("src/foo.rs", "");

        let mut diagnostics = Vec::new();
        let err = Walker::new(&tree.0, &mut diagnostics)
            .walk(&tree.0.join("src/lib.rs"), "demo", &mut |_| {
                anyhow::bail!("consumer said no")
            })
            .expect_err("the consumer's error must propagate");
        assert!(err.to_string().contains("consumer said no"), "{err}");
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
                ("demo".to_string(), "src/lib.rs".to_string()),
                ("demo::foo".to_string(), "src/foo.rs".to_string()),
            ]
        );
        assert!(diagnostics.is_empty(), "{:?}", rendered(&diagnostics));
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
                ("demo".to_string(), "src/lib.rs".to_string()),
                ("demo::foo".to_string(), "src/foo/mod.rs".to_string()),
                ("demo::foo::bar".to_string(), "src/foo/bar.rs".to_string()),
                (
                    "demo::foo::bar::baz".to_string(),
                    "src/foo/bar/baz.rs".to_string()
                ),
            ]
        );
        assert!(diagnostics.is_empty(), "{:?}", rendered(&diagnostics));
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
                ("demo".to_string(), "src/lib.rs".to_string()),
                (
                    "demo::aliased".to_string(),
                    "src/elsewhere/renamed.rs".to_string()
                ),
                (
                    "demo::aliased::child".to_string(),
                    "src/elsewhere/child.rs".to_string()
                ),
            ]
        );
        assert!(diagnostics.is_empty(), "{:?}", rendered(&diagnostics));
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

        assert_eq!(modules(&files), vec!["tool", "tool::helper"], "{files:?}");
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
        assert_eq!(diagnostics.len(), 1, "{:?}", rendered(&diagnostics));
        let line = diagnostics[0].to_string();
        assert!(line.contains("demo::absent"), "{line}");
        assert!(line.contains("src/absent.rs"), "{line}");
        assert!(line.contains("src/absent/mod.rs"), "{line}");
        // C18 counts it: the module is simply not in the report.
        assert!(diagnostics[0].kind.declines_analysis(), "{line}");
    }

    /// Both files present is an error to rustc; here it is a warning naming
    /// both, so which one the numbers describe is never a guess. It is *not* a
    /// decline: one of them was analysed (C18).
    #[test]
    fn a_module_with_two_candidate_files_is_diagnosed() {
        let tree = Tree::new("ambiguous_module");
        tree.write("src/lib.rs", "mod foo;\n")
            .write("src/foo.rs", "")
            .write("src/foo/mod.rs", "");

        let (files, diagnostics) = tree.walk("demo", "src/lib.rs");

        assert_eq!(modules(&files), vec!["demo", "demo::foo"], "{files:?}");
        assert_eq!(diagnostics.len(), 1, "{:?}", rendered(&diagnostics));
        let line = diagnostics[0].to_string();
        assert!(line.contains("src/foo.rs"), "{line}");
        assert!(line.contains("src/foo/mod.rs"), "{line}");
        assert!(!diagnostics[0].kind.declines_analysis(), "{line}");
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
        assert!(diagnostics.is_empty(), "{:?}", rendered(&diagnostics));
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
        assert_eq!(diagnostics.len(), 1, "{:?}", rendered(&diagnostics));
        let line = diagnostics[0].to_string();
        assert!(line.contains("demo::imp"), "{line}");
        assert!(line.contains("cfg_attr"), "{line}");
        // The line names the declaration (file:line:column) and the predicate
        // that was not evaluated — enough for a reader to go and look.
        assert!(line.contains("src/lib.rs:2:5"), "{line}");
        assert!(line.contains("path = \"windows.rs\""), "{line}");
        // The module and its whole subtree are unmeasured, so C18 counts it.
        assert!(diagnostics[0].kind.declines_analysis(), "{line}");
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
        assert_eq!(diagnostics.len(), 1, "{:?}", rendered(&diagnostics));
        assert!(
            diagnostics[0].to_string().contains("demo::outer"),
            "{:?}",
            rendered(&diagnostics)
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

        let (_, diagnostics) = tree.walk("demo", "src/lib.rs");

        let lines = rendered(&diagnostics);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(
            lines[0].contains("src/lib.rs:3:5") && lines[0].contains("path = \"unix.rs\""),
            "{lines:?}"
        );
        assert!(
            lines[1].contains("src/lib.rs:6:5") && lines[1].contains("path = \"windows.rs\""),
            "{lines:?}"
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

        let root = tree.0.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut diagnostics = Vec::new();
            let mut walked = 0;
            let result = Walker::new(&root, &mut diagnostics).walk(
                &root.join("src/lib.rs"),
                "demo",
                &mut |_| {
                    walked += 1;
                    Ok(())
                },
            );
            let _ = sender.send(result.map(|()| walked));
        });

        match receiver.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(walked) => assert_eq!(walked.expect("walk the graph"), 1),
            Err(_) => panic!("the module-graph walk did not terminate"),
        }
    }

    /// A file cargo says it compiles but which cannot be resolved on disk is an
    /// operational error, not a decline: there is no bounded answer to give.
    #[test]
    fn an_unresolvable_root_is_an_operational_error() {
        let tree = Tree::new("absent_root");
        let mut diagnostics = Vec::new();
        let err = Walker::new(&tree.0, &mut diagnostics)
            .walk(&tree.0.join("src/absent.rs"), "demo", &mut |_| Ok(()))
            .expect_err("an absent root cannot be walked");
        assert!(
            err.to_string().contains("failed to resolve source file"),
            "{err}"
        );
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
}
