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
    /// Simple (unqualified) function name. Full identity/naming is task T2.
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
    };
    collector.visit_file(file);
    collector.results
}

/// Walks the whole AST recording one [`FunctionComplexity`] per function
/// definition. Recursion continues into each function body so that nested `fn`
/// items are recorded as their own functions.
struct FnCollector {
    results: Vec<FunctionComplexity>,
}

impl FnCollector {
    fn record(&mut self, name: &syn::Ident, block: &syn::Block, span: proc_macro2::Span) {
        self.results.push(FunctionComplexity {
            name: name.to_string(),
            complexity: complexity_of_block(block),
            start_line: span.start().line,
            end_line: span.end().line,
        });
    }
}

impl<'ast> Visit<'ast> for FnCollector {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        self.record(&node.sig.ident, &node.block, node.span());
        visit::visit_item_fn(self, node);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.record(&node.sig.ident, &node.block, node.span());
        visit::visit_impl_item_fn(self, node);
    }

    fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
        // Only provided (default-bodied) trait methods have a function to measure.
        if let Some(block) = &node.default {
            self.record(&node.sig.ident, block, node.span());
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
        assert!(fns.iter().all(|f| f.name != "required"));
        assert_eq!(
            fns.iter()
                .find(|f| f.name == "provided")
                .unwrap()
                .complexity,
            2
        );
        // match with one real arm + one catch-all → base 1 + 1 = 2.
        assert_eq!(
            fns.iter().find(|f| f.name == "method").unwrap().complexity,
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
}
