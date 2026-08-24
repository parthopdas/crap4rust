//! Pure, I/O-free cyclomatic-complexity (CC) engine.
//!
//! Given Rust source (or a parsed `syn` AST), it computes the cyclomatic
//! complexity of every function it finds, along with each function's source
//! line span (needed later to join with coverage).
//!
//! **Source-level only.** Analysis is over surface syntax via `syn`: macros are
//! *not* expanded, so control flow generated inside a macro body is invisible
//! here. `async`/`.await` desugaring is likewise not modelled — `.await` is not
//! itself a decision point. These are deliberate, documented limitations (C12).
//!
//! The public surface below is consumed by later pipeline tasks (naming/identity,
//! CRAP join, CLI). Until the CLI is wired it is exercised only by unit tests, so
//! `dead_code` is allowed here rather than littering each item with `#[allow]`.
#![allow(dead_code)]

use syn::spanned::Spanned;
use syn::visit::{self, Visit};

/// Cyclomatic complexity plus source span for a single analysed function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FunctionComplexity {
    /// Qualified display name assembled from enclosing context: module path for
    /// free/nested `fn`s (`bar::foo`), `Type::method` for inherent impls,
    /// `<Type as Trait>::method` for trait impls, and `Trait::method` for
    /// provided trait methods. Cross-crate qualification is task T9.
    pub(crate) name: String,
    /// Cyclomatic complexity: base 1 plus one per decision point.
    pub(crate) complexity: u32,
    /// 1-based start line of the function in source.
    pub(crate) start_line: usize,
    /// 1-based end line of the function in source.
    pub(crate) end_line: usize,
}

/// Parse `src` as a Rust source file and compute the cyclomatic complexity of
/// every function it defines.
pub(crate) fn analyze_str(src: &str) -> syn::Result<Vec<FunctionComplexity>> {
    Ok(analyze_file(&syn::parse_file(src)?))
}

/// Compute the cyclomatic complexity of every function in an already-parsed file.
///
/// "Every function" means free `fn`s, inherent/trait-impl methods, provided
/// (default-bodied) trait methods, and nested `fn` items — each nested `fn` is
/// its own function. Closures are *not* separate functions: their decisions are
/// attributed to the enclosing named function.
pub(crate) fn analyze_file(file: &syn::File) -> Vec<FunctionComplexity> {
    let mut collector = FnCollector {
        results: Vec::new(),
        ctx: Vec::new(),
    };
    collector.visit_file(file);
    collector.results
}

/// One frame of enclosing context, pushed/popped as the visitor descends so a
/// recorded function can assemble its qualified name.
enum Ctx {
    /// `mod <name>` — contributes a module-path segment.
    Mod(String),
    /// `impl <self_ty>` or `impl <trait_path> for <self_ty>`.
    Impl {
        self_ty: String,
        trait_path: Option<String>,
    },
    /// `trait <name>` — the receiver for its provided methods.
    Trait(String),
}

/// Walks the whole AST recording one [`FunctionComplexity`] per function
/// definition. Recursion continues into each function body so that nested `fn`
/// items are recorded as their own functions. A context stack ([`Ctx`]) tracks
/// enclosing `mod`/`impl`/`trait` frames to build qualified names.
struct FnCollector {
    results: Vec<FunctionComplexity>,
    ctx: Vec<Ctx>,
}

impl FnCollector {
    fn record(&mut self, name: String, block: &syn::Block, span: proc_macro2::Span) {
        self.results.push(FunctionComplexity {
            name,
            complexity: complexity_of_block(block),
            start_line: span.start().line,
            end_line: span.end().line,
        });
    }

    /// Enclosing module-path segments (impl/trait frames excluded).
    fn module_segments(&self) -> Vec<String> {
        self.ctx
            .iter()
            .filter_map(|c| match c {
                Ctx::Mod(name) => Some(name.clone()),
                _ => None,
            })
            .collect()
    }

    /// Receiver prefix for a method, taken from the innermost `impl`/`trait`
    /// frame (always the immediate parent of a method item).
    fn method_receiver(&self) -> Option<String> {
        match self.ctx.last() {
            Some(Ctx::Impl {
                self_ty,
                trait_path: Some(trait_path),
            }) => Some(format!("<{self_ty} as {trait_path}>")),
            Some(Ctx::Impl {
                self_ty,
                trait_path: None,
            }) => Some(self_ty.clone()),
            Some(Ctx::Trait(name)) => Some(name.clone()),
            _ => None,
        }
    }

    /// Qualified name for a free/nested `fn`: module path plus its own name.
    fn free_name(&self, ident: &syn::Ident) -> String {
        let mut segs = self.module_segments();
        segs.push(ident.to_string());
        segs.join("::")
    }

    /// Qualified name for an impl/trait method: module path, receiver, own name.
    fn method_name(&self, ident: &syn::Ident) -> String {
        let mut segs = self.module_segments();
        segs.extend(self.method_receiver());
        segs.push(ident.to_string());
        segs.join("::")
    }
}

/// Render a `syn::Path` as `seg::seg`, dropping generic arguments/lifetimes.
fn path_name(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|seg| seg.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

/// Render an `impl` self-type as a clean name, stripping generics/lifetimes and
/// peeling references so `impl Foo<T>`/`impl &Foo` both read as `Foo`.
fn self_type_name(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Path(tp) => path_name(&tp.path),
        syn::Type::Reference(r) => self_type_name(&r.elem),
        syn::Type::Group(g) => self_type_name(&g.elem),
        syn::Type::Paren(p) => self_type_name(&p.elem),
        _ => "_".to_string(),
    }
}

impl<'ast> Visit<'ast> for FnCollector {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        self.ctx.push(Ctx::Mod(node.ident.to_string()));
        visit::visit_item_mod(self, node);
        self.ctx.pop();
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        self.ctx.push(Ctx::Impl {
            self_ty: self_type_name(&node.self_ty),
            trait_path: node.trait_.as_ref().map(|(_, path, _)| path_name(path)),
        });
        visit::visit_item_impl(self, node);
        self.ctx.pop();
    }

    fn visit_item_trait(&mut self, node: &'ast syn::ItemTrait) {
        self.ctx.push(Ctx::Trait(node.ident.to_string()));
        visit::visit_item_trait(self, node);
        self.ctx.pop();
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        let name = self.free_name(&node.sig.ident);
        self.record(name, &node.block, node.span());
        visit::visit_item_fn(self, node);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        let name = self.method_name(&node.sig.ident);
        self.record(name, &node.block, node.span());
        visit::visit_impl_item_fn(self, node);
    }

    fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
        // Only provided (default-bodied) trait methods have a function to measure.
        if let Some(block) = &node.default {
            let name = self.method_name(&node.sig.ident);
            self.record(name, block, node.span());
            visit::visit_trait_item_fn(self, node);
        }
    }
}

/// Cyclomatic complexity of a single function body: base 1 plus its decisions.
fn complexity_of_block(block: &syn::Block) -> u32 {
    let mut counter = CcCounter { count: 1 };
    counter.visit_block(block);
    counter.count
}

/// Counts decision points within one function body.
///
/// It descends into closures (their decisions roll up into this function) but
/// stops at nested item definitions — a nested `fn`/`impl`/`mod` is a separate
/// scope, measured independently by [`FnCollector`].
struct CcCounter {
    count: u32,
}

impl<'ast> Visit<'ast> for CcCounter {
    /// Do not descend into nested item definitions; they are their own functions.
    fn visit_item(&mut self, _node: &'ast syn::Item) {}

    /// `if`, `else if`, and `if let` each add one (a bare `else` is not an
    /// `ExprIf`, so it correctly adds nothing).
    fn visit_expr_if(&mut self, node: &'ast syn::ExprIf) {
        self.count += 1;
        visit::visit_expr_if(self, node);
    }

    /// Covers both `while` and `while let` (its condition is an `ExprLet`).
    fn visit_expr_while(&mut self, node: &'ast syn::ExprWhile) {
        self.count += 1;
        visit::visit_expr_while(self, node);
    }

    fn visit_expr_for_loop(&mut self, node: &'ast syn::ExprForLoop) {
        self.count += 1;
        visit::visit_expr_for_loop(self, node);
    }

    fn visit_expr_loop(&mut self, node: &'ast syn::ExprLoop) {
        self.count += 1;
        visit::visit_expr_loop(self, node);
    }

    /// Each arm whose pattern is not the wildcard `_` adds one; a match-arm
    /// guard adds one more, separately from (and independent of) its pattern.
    /// So `_ if cond` = +1 (guard only; `_` never counts).
    fn visit_expr_match(&mut self, node: &'ast syn::ExprMatch) {
        for arm in &node.arms {
            if !matches!(arm.pat, syn::Pat::Wild(_)) {
                self.count += 1;
            }
            if arm.guard.is_some() {
                self.count += 1;
            }
        }
        visit::visit_expr_match(self, node);
    }

    /// Short-circuiting `&&` and `||` each add one (bitwise `&`/`|` do not).
    fn visit_expr_binary(&mut self, node: &'ast syn::ExprBinary) {
        if matches!(node.op, syn::BinOp::And(_) | syn::BinOp::Or(_)) {
            self.count += 1;
        }
        visit::visit_expr_binary(self, node);
    }

    /// The `?` try operator adds one per occurrence.
    fn visit_expr_try(&mut self, node: &'ast syn::ExprTry) {
        self.count += 1;
        visit::visit_expr_try(self, node);
    }

    /// A `let ... else { }` (the diverging `else`) adds one.
    fn visit_local(&mut self, node: &'ast syn::Local) {
        if node
            .init
            .as_ref()
            .and_then(|init| init.diverge.as_ref())
            .is_some()
        {
            self.count += 1;
        }
        visit::visit_local(self, node);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Analyse `src` and return the complexity of its single function.
    fn cc(src: &str) -> u32 {
        let mut fns = analyze_str(src).expect("source should parse");
        assert_eq!(fns.len(), 1, "expected exactly one function in fixture");
        fns.remove(0).complexity
    }

    #[test]
    fn per_construct_complexity() {
        struct Case {
            name: &'static str,
            src: &'static str,
            expected: u32,
        }

        let cases = [
            Case {
                name: "empty fn",
                src: "fn f() {}",
                expected: 1,
            },
            Case {
                name: "straight-line fn",
                src: "fn f() { let a = 1; let b = a + 2; println!(\"{b}\"); }",
                expected: 1,
            },
            Case {
                name: "single if",
                src: "fn f(x: i32) { if x > 0 {} }",
                expected: 2,
            },
            Case {
                name: "if / else-if chain, bare else does not count",
                src: "fn f(x: i32) { if x > 0 {} else if x < 0 {} else {} }",
                expected: 3,
            },
            Case {
                name: "single wildcard arm: only `_` is exempt",
                src: "fn f(x: i32) { match x { _ => {} } }",
                expected: 1,
            },
            Case {
                name: "match with `_`: two literal arms count, `_` exempt",
                src: "fn f(x: i32) { match x { 1 => {}, 2 => {}, _ => {} } }",
                expected: 3,
            },
            Case {
                name: "bare-binding arm counts: only `_` is exempt (option i)",
                src: "fn f(x: i32) { match x { 1 => {}, other => { let _ = other; } } }",
                expected: 3,
            },
            Case {
                name: "match without `_`: every arm counts",
                src: "fn f(x: i32) { match x { 1 => {}, 2 => {}, 3 => {} } }",
                expected: 4,
            },
            Case {
                name: "unit-variant arms count: None no longer exempt",
                src: "fn f(o: Option<i32>) { match o { Some(v) => { let _ = v; }, None => {} } }",
                expected: 3,
            },
            Case {
                name: "Ok/Err arms both count",
                src: "fn f(r: Result<i32, i32>) { match r { Ok(v) => { let _ = v; }, Err(e) => { let _ = e; } } }",
                expected: 3,
            },
            Case {
                name: "bare unit-variant + bare binding both count; only `_` exempt",
                src: "fn f(x: E) { match x { Foo => {}, Bar => {}, other => { let _ = other; } } }",
                expected: 4,
            },
            Case {
                name: "literal + bare binding both count; only `_` exempt",
                src: "fn f(n: i32) { match n { 0 => {}, rest => { let _ = rest; } } }",
                expected: 3,
            },
            Case {
                name: "logical &&",
                src: "fn f(a: bool, b: bool) -> bool { a && b }",
                expected: 2,
            },
            Case {
                name: "logical || and &&",
                src: "fn f(a: bool, b: bool, c: bool) -> bool { a && b || c }",
                expected: 3,
            },
            Case {
                name: "bitwise & and | do not count",
                src: "fn f(a: u8, b: u8) -> u8 { a & b | a }",
                expected: 1,
            },
            Case {
                name: "if let",
                src: "fn f(x: Option<i32>) { if let Some(v) = x { let _ = v; } }",
                expected: 2,
            },
            Case {
                name: "let-else",
                src: "fn f(x: Option<i32>) { let Some(v) = x else { return; }; let _ = v; }",
                expected: 2,
            },
            Case {
                name: "while",
                src: "fn f(mut x: i32) { while x > 0 { x -= 1; } }",
                expected: 2,
            },
            Case {
                name: "while let",
                src: "fn f(mut it: std::vec::IntoIter<i32>) { while let Some(v) = it.next() { let _ = v; } }",
                expected: 2,
            },
            Case {
                name: "for",
                src: "fn f() { for i in 0..10 { let _ = i; } }",
                expected: 2,
            },
            Case {
                name: "loop",
                src: "fn f() { loop { break; } }",
                expected: 2,
            },
            Case {
                name: "try operator ?",
                src: "fn f(x: Option<i32>) -> Option<i32> { let a = x?; let b = x?; Some(a + b) }",
                expected: 3,
            },
            Case {
                name: "match-arm guard counted separately from its (binding) pattern",
                src: "fn f(x: i32) { match x { n if n > 0 => {}, _ => {} } }",
                expected: 3,
            },
            Case {
                name: "guarded wildcard `_ if cond` = +1 (guard only; `_` +0)",
                src: "fn f(x: i32) { match x { _ if x > 0 => {}, _ => {} } }",
                expected: 2,
            },
            Case {
                name: "two guarded wildcards = base 1 + two guards; `_` patterns +0",
                src: "fn f(x: i32) { match x { _ if x > 0 => {}, _ if x < 0 => {}, _ => {} } }",
                expected: 3,
            },
            Case {
                name: "guarded refutable pattern = pattern +1 and guard +1",
                src: "fn f(x: Option<i32>) { match x { Some(y) if y > 0 => { let _ = y; }, _ => {} } }",
                expected: 3,
            },
        ];

        for case in cases {
            assert_eq!(cc(case.src), case.expected, "case: {}", case.name);
        }
    }

    #[test]
    fn closure_decisions_roll_up_into_enclosing_fn() {
        // base 1 + closure's `if` (1) + closure's `&&` (1) = 3, all on the parent.
        let src = r#"
            fn f(items: Vec<i32>) -> Vec<i32> {
                items
                    .into_iter()
                    .filter(|&n| if n > 0 && n < 10 { true } else { false })
                    .collect()
            }
        "#;
        assert_eq!(cc(src), 3);
    }

    #[test]
    fn nested_fn_is_its_own_function() {
        let src = r#"
            fn outer(x: i32) -> i32 {
                fn inner(y: i32) -> i32 {
                    if y > 0 {
                        y
                    } else {
                        -y
                    }
                }
                if x > 0 {
                    inner(x)
                } else {
                    0
                }
            }
        "#;
        let fns = analyze_str(src).expect("parses");
        let outer = fns.iter().find(|f| f.name == "outer").expect("outer");
        let inner = fns.iter().find(|f| f.name == "inner").expect("inner");
        // Each has base 1 + its own single `if`; the nested fn does not inflate outer.
        assert_eq!(
            outer.complexity, 2,
            "outer should not absorb inner's decisions"
        );
        assert_eq!(inner.complexity, 2);
    }

    #[test]
    fn impl_and_trait_methods_are_measured() {
        let src = r#"
            trait T {
                fn required(&self);
                fn provided(&self, x: i32) -> i32 {
                    if x > 0 { x } else { -x }
                }
            }
            struct S;
            impl S {
                fn method(&self, x: i32) -> i32 {
                    match x {
                        0 => 0,
                        _ => 1,
                    }
                }
            }
        "#;
        let fns = analyze_str(src).expect("parses");
        // `required` has no body → not measured.
        assert!(fns.iter().all(|f| f.name != "T::required"));
        assert_eq!(
            fns.iter()
                .find(|f| f.name == "T::provided")
                .unwrap()
                .complexity,
            2
        );
        // match with one real arm + one catch-all → base 1 + 1 = 2.
        assert_eq!(
            fns.iter()
                .find(|f| f.name == "S::method")
                .unwrap()
                .complexity,
            2
        );
    }

    #[test]
    fn combined_realistic_functions() {
        // base 1
        // + `for` (1)
        // + `if x > 0 && x % 2 == 0` (if 1, && 1)
        // + `?` (1)
        // = 5
        let classify = r#"
            fn classify(xs: Vec<i32>) -> Result<i32, ()> {
                let mut total = 0;
                for x in xs {
                    if x > 0 && x % 2 == 0 {
                        total += x;
                    }
                }
                let doubled = check(total)?;
                Ok(doubled)
            }
        "#;
        assert_eq!(cc(classify), 5);

        // base 1
        // + match arms: `Some(0)` (1), `Some(n) if n > 0` (arm 1 + guard 1 = 2),
        //   `Some(_)` (1 — a tuple-struct pattern, NOT a catch-all), `_` (0 — catch-all) = 4
        // + `while let` (1)
        // = 6
        let handle = r#"
            fn handle(v: Option<i32>) -> i32 {
                let mut acc = match v {
                    Some(0) => 0,
                    Some(n) if n > 0 => n,
                    Some(_) => -2,
                    _ => -1,
                };
                let mut it = (0..acc.max(0)).into_iter();
                while let Some(step) = it.next() {
                    acc += step;
                }
                acc
            }
        "#;
        assert_eq!(cc(handle), 6);
    }

    #[test]
    fn spans_are_captured() {
        let src = "fn a() {}\n\nfn b() {\n    let _ = 1;\n}\n";
        let fns = analyze_str(src).expect("parses");
        let a = fns.iter().find(|f| f.name == "a").unwrap();
        let b = fns.iter().find(|f| f.name == "b").unwrap();
        assert_eq!((a.start_line, a.end_line), (1, 1));
        assert_eq!((b.start_line, b.end_line), (3, 5));
    }

    #[test]
    fn qualified_names_track_enclosing_context() {
        struct Case {
            src: &'static str,
            expected: &'static str,
        }

        let cases = [
            // Free fn at crate root → bare name.
            Case {
                src: "fn foo() {}",
                expected: "foo",
            },
            // Free fn inside a nested module → module-path qualified.
            Case {
                src: "mod bar { fn foo() {} }",
                expected: "bar::foo",
            },
            // Inherent impl method → Type::method.
            Case {
                src: "struct Foo; impl Foo { fn bar(&self) {} }",
                expected: "Foo::bar",
            },
            // Trait-impl method → <Type as Trait>::method.
            Case {
                src: "struct Foo; trait Tr { fn bar(&self); } impl Tr for Foo { fn bar(&self) {} }",
                expected: "<Foo as Tr>::bar",
            },
            // Provided (default-bodied) trait method → Trait::method.
            Case {
                src: "trait T { fn m(&self) {} }",
                expected: "T::m",
            },
            // Module + impl combine → m::Type::method.
            Case {
                src: "mod m { struct Foo; impl Foo { fn bar(&self) {} } }",
                expected: "m::Foo::bar",
            },
            // Generics on the self-type are stripped for a clean name.
            Case {
                src: "struct Foo<T>(T); impl<T> Foo<T> { fn bar(&self) {} }",
                expected: "Foo::bar",
            },
        ];

        for case in cases {
            let fns = analyze_str(case.src).expect("source should parse");
            assert!(
                fns.iter().any(|f| f.name == case.expected),
                "expected name {:?} among {:?}",
                case.expected,
                fns.iter().map(|f| f.name.as_str()).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn nested_fn_is_qualified_by_module_only() {
        // At crate root: nested fn keeps only its own name (parent fn excluded).
        let fns = analyze_str("fn outer() { fn inner() {} }").expect("parses");
        assert!(fns.iter().any(|f| f.name == "outer"));
        assert!(fns.iter().any(|f| f.name == "inner"));

        // Inside a module: module-qualified, parent fn name still excluded.
        let fns = analyze_str("mod m { fn outer() { fn inner() {} } }").expect("parses");
        assert!(fns.iter().any(|f| f.name == "m::outer"));
        assert!(fns.iter().any(|f| f.name == "m::inner"));

        // Inside an impl method: qualified by module path only, not the impl type.
        let fns = analyze_str("struct Foo; impl Foo { fn bar(&self) { fn inner() {} } }")
            .expect("parses");
        assert!(fns.iter().any(|f| f.name == "Foo::bar"));
        assert!(fns.iter().any(|f| f.name == "inner"));
    }

    #[test]
    fn sibling_contexts_do_not_leak_names() {
        // Guards the push/visit/pop invariant: after visiting one frame, its
        // segment must be popped before the sibling is visited — no leakage.
        let fns = analyze_str("mod a { fn f() {} } mod b { fn f() {} }").expect("parses");
        let names: Vec<&str> = fns.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"a::f"), "expected a::f among {names:?}");
        assert!(names.contains(&"b::f"), "expected b::f among {names:?}");

        let fns =
            analyze_str("struct A; struct B; impl A { fn m(&self) {} } impl B { fn m(&self) {} }")
                .expect("parses");
        let names: Vec<&str> = fns.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"A::m"), "expected A::m among {names:?}");
        assert!(names.contains(&"B::m"), "expected B::m among {names:?}");
    }

    #[test]
    fn multi_level_module_nesting_accumulates_all_segments() {
        // Pins multi-frame accumulation and pop ordering across nested modules.
        let fns = analyze_str("mod a { mod b { fn f() {} } }").expect("parses");
        let names: Vec<&str> = fns.iter().map(|f| f.name.as_str()).collect();
        assert!(
            names.contains(&"a::b::f"),
            "expected a::b::f among {names:?}"
        );
    }

    #[test]
    fn or_pattern_arm_counts_once_not_per_alternative() {
        // The or-pattern arm `1 | 2 | 3` counts +1 for the arm (NOT +1 per
        // alternative); `_` is exempt → base 1 + 1 = 2.
        assert_eq!(
            cc("fn f(x: i32) -> i32 { match x { 1 | 2 | 3 => 0, _ => 1 } }"),
            2
        );
    }

    #[test]
    fn async_fn_await_adds_nothing() {
        // Source-level analysis (C12): `.await` is not a decision point, so only
        // the `if` counts → base 1 + 1 = 2.
        let src = "async fn f(x: i32) -> i32 { if x > 0 { g().await } else { 0 } }";
        assert_eq!(cc(src), 2);
    }
}
