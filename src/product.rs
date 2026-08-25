//! Product-vs-test classification (C5) — pure, I/O-free.
//!
//! One question, asked from two places: is this item part of the **product**,
//! or does it only exist under `cargo test`? The CC engine asks it to decide
//! what to measure ([`crate::complexity`]), and the module-graph walk asks it
//! to decide what to descend into ([`crate::workspace`]) — both per
//! declaration, so a `#[cfg(test)] mod tests;` is not even *resolved*, and per
//! whole file ([`is_test_only_file`]), so a `#![cfg(test)]` file yields no unit
//! to stake a claim on a coverage record **and** none of its `mod`
//! declarations are resolved either. Either way a missing test file must not
//! become a declined module and inflate the C18 count on the artifact
//! (FC-T9g).
//! Both call this, so "what is test code" has exactly one definition.
//!
//! **The evaluation rule, stated once — cited, not restated, elsewhere.** *We
//! evaluate nothing whose truth depends on an environment we do not have.*
//! "`modgraph` evaluates no `cfg`" was always shorthand for this, and shorthand
//! re-argued at three sites is exactly where a policy drifts, so it is named
//! here and referred to from there.
//!
//! The rule cuts both ways, which is the point. `#[cfg(feature = "x")]` is
//! product source under one feature set and not under another and we do not
//! know the feature set, so it is not evaluated — and a conditionally-*pathed*
//! module, whose *file* depends on that same unknown, is declined rather than
//! guessed at ([`crate::workspace`]). But `test` depends on no environment we
//! lack: `cargo test` sets it and a normal build never does, so an item that
//! **requires** `test` is unambiguously not in the product, in every
//! environment there is. Resolving it is the rule applied, not an exception to
//! it — [`requires_test`] answers "is this predicate false in every non-test
//! build", and anything it cannot prove counts as product.
//!
//! Erring towards product is the safe direction, the same one C1's match-arm
//! rule takes: a test helper mistaken for product is *reported* (visibly, with
//! its own row), while product mistaken for a test silently vanishes from the
//! report.

use syn::punctuated::Punctuated;
use syn::Token;

/// `true` when the attributes mark an item that a normal `cargo build` never
/// compiles: a `#[test]` function, or an item behind a `cfg` that requires
/// `test` (C5).
///
/// Only the literal `#[test]` attribute is recognised, not attribute macros
/// that expand to one (`#[tokio::test]` and friends): those live inside a
/// `#[cfg(test)] mod tests` in practice, which is dropped whole, and a stray
/// one is reported rather than silently dropped.
pub(crate) fn is_test_only(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(is_test_attribute)
}

/// `true` when this item is test-only, whatever kind of item it is.
///
/// Asked per *container* rather than per function: a `cfg` belongs to the item
/// it is written on and is **not** inherited into the attribute lists of the
/// items nested inside it, so a `#[cfg(test)] trait`'s methods carry no
/// attribute of their own to be recognised by. Answering the question at every
/// item, whatever its kind, is what makes "skipped whole, subtree and all"
/// structural instead of a list of the kinds someone remembered.
pub(crate) fn is_test_only_item(item: &syn::Item) -> bool {
    is_test_only(item_attrs(item))
}

/// [`is_test_only_item`] for the members of an `impl` block.
pub(crate) fn is_test_only_impl_item(item: &syn::ImplItem) -> bool {
    is_test_only(match item {
        syn::ImplItem::Const(i) => &i.attrs,
        syn::ImplItem::Fn(i) => &i.attrs,
        syn::ImplItem::Macro(i) => &i.attrs,
        syn::ImplItem::Type(i) => &i.attrs,
        _ => &[],
    })
}

/// [`is_test_only_item`] for the members of a `trait` definition.
pub(crate) fn is_test_only_trait_item(item: &syn::TraitItem) -> bool {
    is_test_only(match item {
        syn::TraitItem::Const(i) => &i.attrs,
        syn::TraitItem::Fn(i) => &i.attrs,
        syn::TraitItem::Macro(i) => &i.attrs,
        syn::TraitItem::Type(i) => &i.attrs,
        _ => &[],
    })
}

/// `true` when a whole **file** exists only under `cargo test`: it is `#![cfg(
/// test)]`, or every item it declares is test-only.
///
/// Two consequences follow, and the walk applies both together (FC-T9g): the
/// file yields no [`crate::join::SourceUnit`], and its `mod` declarations are
/// not resolved. A file of nothing but `#[cfg(test)]` code is not part of the
/// product and must stake no claim on an LCOV record; neither may the children
/// it declares, which a non-test build never compiles at all.
///
/// The test is "is this unit test-only", **not** "did it yield any functions":
/// a product file that happens to declare only constants, types or modules has
/// zero functions but is still product, and dropping it would let another file
/// consume a record that may describe *this* one.
///
/// An empty file is not called test-only: there is nothing in it to prove the
/// claim with, and product is the safe direction (a file wrongly kept is at
/// worst a contested record reported as `N/A`, while a file wrongly dropped
/// silently hands its record to someone else).
pub(crate) fn is_test_only_file(file: &syn::File) -> bool {
    is_test_only(&file.attrs)
        || (!file.items.is_empty() && file.items.iter().all(is_test_only_item))
}

/// The attributes of any item kind.
///
/// Kinds that carry none — and any variant added to `syn::Item` in future —
/// read as attribute-free, so they cannot be *proven* test-only and count as
/// product.
fn item_attrs(item: &syn::Item) -> &[syn::Attribute] {
    match item {
        syn::Item::Const(i) => &i.attrs,
        syn::Item::Enum(i) => &i.attrs,
        syn::Item::ExternCrate(i) => &i.attrs,
        syn::Item::Fn(i) => &i.attrs,
        syn::Item::ForeignMod(i) => &i.attrs,
        syn::Item::Impl(i) => &i.attrs,
        syn::Item::Macro(i) => &i.attrs,
        syn::Item::Mod(i) => &i.attrs,
        syn::Item::Static(i) => &i.attrs,
        syn::Item::Struct(i) => &i.attrs,
        syn::Item::Trait(i) => &i.attrs,
        syn::Item::TraitAlias(i) => &i.attrs,
        syn::Item::Type(i) => &i.attrs,
        syn::Item::Union(i) => &i.attrs,
        syn::Item::Use(i) => &i.attrs,
        _ => &[],
    }
}

/// `true` for `#[test]`, and for a `#[cfg(..)]` whose predicate requires `test`.
fn is_test_attribute(attr: &syn::Attribute) -> bool {
    if attr.path().is_ident("test") {
        return true;
    }
    if !attr.path().is_ident("cfg") {
        return false;
    }
    match &attr.meta {
        syn::Meta::List(list) => list
            .parse_args::<syn::Meta>()
            .is_ok_and(|m| requires_test(&m)),
        _ => false,
    }
}

/// `true` when this `cfg` predicate is false in **every** build that does not
/// set `test` — i.e. the item it guards is not product source.
///
/// `all(..)` requires `test` as soon as one of its terms does; `any(..)` only
/// when *every* alternative does (`any(test, unix)` is real product source on
/// unix). Everything else — `not(..)`, features, target predicates, and any
/// predicate we cannot parse — is treated as product: we only drop what we can
/// prove is test-only.
fn requires_test(meta: &syn::Meta) -> bool {
    if let syn::Meta::Path(path) = meta {
        return path.is_ident("test");
    }
    let syn::Meta::List(list) = meta else {
        return false;
    };
    let Ok(terms) = list.parse_args_with(Punctuated::<syn::Meta, Token![,]>::parse_terminated)
    else {
        return false;
    };
    if list.path.is_ident("all") {
        terms.iter().any(requires_test)
    } else if list.path.is_ident("any") {
        !terms.is_empty() && terms.iter().all(requires_test)
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Attributes of the first item of a parsed file.
    fn attrs(src: &str) -> Vec<syn::Attribute> {
        let file = syn::parse_file(src).expect("valid source");
        match file.items.first().expect("one item") {
            syn::Item::Fn(item) => item.attrs.clone(),
            syn::Item::Mod(item) => item.attrs.clone(),
            syn::Item::Impl(item) => item.attrs.clone(),
            _ => panic!("unsupported item in: {src}"),
        }
    }

    fn test_only(src: &str) -> bool {
        is_test_only(&attrs(src))
    }

    #[test]
    fn test_attributes_and_cfg_test_are_test_only() {
        assert!(test_only("#[test]\nfn t() {}\n"));
        assert!(test_only("#[cfg(test)]\nmod tests;\n"));
        assert!(test_only("#[cfg(test)]\nfn helper() {}\n"));
        assert!(test_only("#[cfg(test)]\nimpl Foo {}\n"));
        // The `cfg` may sit among other attributes.
        assert!(test_only("#[derive(Debug)]\n#[cfg(test)]\nmod tests;\n"));
    }

    #[test]
    fn a_predicate_that_requires_test_is_test_only() {
        assert!(test_only("#[cfg(all(test, unix))]\nmod tests;\n"));
        assert!(test_only(
            "#[cfg(any(test, all(test, unix)))]\nmod tests;\n"
        ));
        assert!(test_only("#[cfg(all(unix, any(test)))]\nmod tests;\n"));
    }

    /// Only what can be *proven* test-only is dropped: everything else is
    /// product, because a product function missing from the report is invisible
    /// while a test helper in it is not.
    #[test]
    fn anything_not_provably_test_only_is_product() {
        assert!(!test_only("fn plain() {}\n"));
        assert!(!test_only("#[cfg(unix)]\nmod imp;\n"));
        assert!(!test_only("#[cfg(feature = \"test\")]\nmod imp;\n"));
        // Real product source on every non-test unix build.
        assert!(!test_only("#[cfg(any(test, unix))]\nmod imp;\n"));
        // `not(test)` is the *opposite* claim: compiled unless testing.
        assert!(!test_only("#[cfg(not(test))]\nmod imp;\n"));
        // An attribute macro that merely expands to `#[test]` is not read as
        // one — the item is reported, not silently dropped.
        assert!(!test_only("#[tokio::test]\nfn t() {}\n"));
        // `#[cfg_attr(test, ..)]` guards an *attribute*, not the item.
        assert!(!test_only("#[cfg_attr(test, derive(Debug))]\nmod imp;\n"));
    }

    fn test_only_file(src: &str) -> bool {
        is_test_only_file(&syn::parse_file(src).expect("valid source"))
    }

    #[test]
    fn a_file_of_nothing_but_test_code_is_test_only() {
        assert!(test_only_file("#![cfg(test)]\nfn helper() {}\n"));
        assert!(test_only_file(
            "#[cfg(test)]\nmod tests {\n    fn t() {}\n}\n"
        ));
        assert!(test_only_file(
            "#[cfg(test)]\nuse std::fmt;\n#[test]\nfn t() {}\n"
        ));
    }

    /// A product file with nothing to measure is still product: it must keep
    /// staking its claim on a coverage record, or a same-named file elsewhere
    /// in the workspace silently inherits it (FC-T9g).
    #[test]
    fn a_file_with_product_items_is_not_test_only() {
        assert!(!test_only_file("pub const LIMIT: u32 = 3;\n"));
        assert!(!test_only_file("mod a;\nmod b;\n"));
        assert!(!test_only_file(
            "pub struct S;\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {}\n}\n"
        ));
        // Nothing in an empty file proves anything, so it counts as product.
        assert!(!test_only_file(""));
    }
}
