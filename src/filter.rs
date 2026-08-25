//! Path-fragment filters (C5/FC-T9j) — pure, I/O-free.
//!
//! **A filter is not the positional `PATH`.** The positional argument is a
//! *locator*: cargo searches it and its ancestors for the manifest, and the
//! whole workspace it finds is analysed (FC-T9j). Narrowing what is *reported*
//! is a different kind of thing, so it has its own repeatable flag: clap cannot
//! tell a locator from a filter positionally, and `crap4rust crates/alpha
//! crates/beta` reading as "locate at alpha, filter to beta" is not defensible.
//!
//! **Matching is by whole path segments.** A fragment matches a file when its
//! `/`-segments appear as a consecutive run of the file's segments, so
//! `crates/alpha` selects `crates/alpha/src/lib.rs` but not
//! `crates/alpha-utils/src/lib.rs`, and `lib.rs` selects every crate root of
//! that name. Substring matching would quietly pull in the neighbouring
//! package, which is exactly the kind of plausible-but-wrong output this tool
//! refuses to produce. Comparison is **case-sensitive on every platform**, so
//! one command means one thing across the CI matrix (C29) — and there is no
//! "did you mean" hint, because fuzzy matching is exactly what this tool
//! refuses to do everywhere else.
//!
//! **A filter narrows the report, never the analysis (C26).** Nothing here
//! decides what is *measured*: the caller analyses and joins the whole
//! workspace and applies these selections to the finished rows, so a file's
//! numbers are identical whether or not a filter was given.

use std::collections::BTreeSet;

/// The `--filter` fragments a run was given, if any.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Filters(Vec<Fragment>);

/// One fragment, kept both as the user spelled it (for diagnostics) and as the
/// segments it is matched by.
#[derive(Debug, PartialEq, Eq)]
struct Fragment {
    text: String,
    segments: Vec<String>,
}

impl Filters {
    /// Take the fragments as given. A fragment is separator-insensitive
    /// (`crates\alpha` and `crates/alpha` are the same filter, since the user
    /// types what their shell completed) but otherwise literal.
    pub(crate) fn new(fragments: &[String]) -> Self {
        Self(
            fragments
                .iter()
                .map(|text| Fragment {
                    text: text.clone(),
                    segments: segments(&text.replace('\\', "/"))
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                })
                .collect(),
        )
    }

    /// `true` when no filter was given, i.e. every file is selected.
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The fragments as the user spelled them, in the order given.
    ///
    /// This is C28's request echo: the JSON document states the *effective
    /// request* so a consumer can tell "no risk was found" from "you narrowed
    /// the report". Spelled, not normalized — the echo answers "what did I
    /// ask for?", and `crates\alpha` is what was asked for even though
    /// `crates/alpha` is what was matched.
    pub(crate) fn fragments(&self) -> Vec<&str> {
        self.0.iter().map(|f| f.text.as_str()).collect()
    }

    /// What the filters have to say about `path`, as a value.
    ///
    /// With no filters everything is selected. Otherwise a file needs one
    /// matching fragment: several `--filter`s are alternatives (union), because
    /// the intersection of two path fragments is almost always empty and would
    /// make a second `--filter` silently empty the report.
    pub(crate) fn select(&self, path: &str) -> Selection {
        if self.is_empty() {
            return Selection {
                selected: true,
                matched: Vec::new(),
            };
        }
        let path = segments(path);
        let matched: Vec<usize> = self
            .0
            .iter()
            .enumerate()
            .filter(|(_, fragment)| contains(&path, &fragment.segments))
            .map(|(index, _)| index)
            .collect();
        Selection {
            selected: !matched.is_empty(),
            matched,
        }
    }

    /// The fragments that selected nothing, as the user spelled them.
    ///
    /// A filter matching nothing produces an empty-looking report that is
    /// indistinguishable from a clean workspace, so it is said out loud — a
    /// mistyped fragment is by far the likeliest cause.
    pub(crate) fn unmatched<'a>(&'a self, matched: &BTreeSet<usize>) -> Vec<&'a str> {
        self.0
            .iter()
            .enumerate()
            .filter(|(index, _)| !matched.contains(index))
            .map(|(_, fragment)| fragment.text.as_str())
            .collect()
    }
}

/// What the filters had to say about **one** path.
///
/// Returned as a value rather than accumulated into a shared set, so the
/// per-file question is answered independently of every other file: the caller
/// folds these, and it is the fold — not the order the answers arrive in —
/// that re-establishes the run's ordering when discovery goes parallel
/// (FC-T9i).
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Selection {
    selected: bool,
    matched: Vec<usize>,
}

impl Selection {
    /// Whether this path reaches the report.
    pub(crate) fn is_selected(&self) -> bool {
        self.selected
    }

    /// The fragments that matched it, ascending. The caller folds these into
    /// the run's matched set, which [`Filters::unmatched`] is read against.
    pub(crate) fn matched(&self) -> &[usize] {
        &self.matched
    }
}

/// A path's non-empty `/`-segments.
fn segments(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
}

/// `true` when `needle` occurs as a consecutive run of segments in `haystack`.
/// An empty needle — `--filter ""` — matches nothing rather than everything: an
/// empty fragment expresses no intent, and reporting it as unmatched says so.
fn contains(haystack: &[&str], needle: &[String]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window.iter().zip(needle).all(|(a, b)| *a == b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filters(fragments: &[&str]) -> Filters {
        Filters::new(
            &fragments
                .iter()
                .map(|f| (*f).to_string())
                .collect::<Vec<_>>(),
        )
    }

    fn selects(filters: &Filters, path: &str) -> bool {
        filters.select(path).is_selected()
    }

    #[test]
    fn no_filters_select_everything() {
        let filters = filters(&[]);
        assert!(filters.is_empty());
        assert!(selects(&filters, "crates/alpha/src/lib.rs"));
    }

    #[test]
    fn a_fragment_matches_a_run_of_whole_segments() {
        assert!(selects(
            &filters(&["crates/alpha"]),
            "crates/alpha/src/lib.rs"
        ));
        assert!(selects(
            &filters(&["crates/alpha"]),
            "crates/alpha/src/foo/bar.rs"
        ));
        // A leading run, a trailing run and an interior run all count.
        assert!(selects(&filters(&["src"]), "crates/alpha/src/lib.rs"));
        assert!(selects(&filters(&["lib.rs"]), "crates/alpha/src/lib.rs"));
    }

    /// The whole reason segments beat substrings: a neighbouring package whose
    /// name merely starts the same must not be dragged in.
    #[test]
    fn a_fragment_does_not_match_a_partial_segment() {
        assert!(!selects(
            &filters(&["crates/alpha"]),
            "crates/alpha-utils/src/lib.rs"
        ));
        assert!(!selects(&filters(&["alph"]), "crates/alpha/src/lib.rs"));
        // Consecutive, not merely present in order.
        assert!(!selects(
            &filters(&["crates/src"]),
            "crates/alpha/src/lib.rs"
        ));
    }

    #[test]
    fn separators_are_normalized_but_case_is_not() {
        assert!(selects(
            &filters(&[r"crates\alpha"]),
            "crates/alpha/src/lib.rs"
        ));
        assert!(selects(
            &filters(&["/crates/alpha/"]),
            "crates/alpha/src/lib.rs"
        ));
        // One command means one thing on every platform.
        assert!(!selects(
            &filters(&["Crates/Alpha"]),
            "crates/alpha/src/lib.rs"
        ));
    }

    /// Several filters are alternatives: an intersection would make a second
    /// `--filter` silently empty the report. Each answer is a value the caller
    /// folds; nothing is recorded behind its back.
    #[test]
    fn several_fragments_are_alternatives_and_each_records_its_matches() {
        let filters = filters(&["crates/alpha", "crates/beta", "crates/gamma"]);
        let mut matched = BTreeSet::new();
        for path in [
            "crates/alpha/src/lib.rs",
            "crates/beta/src/lib.rs",
            "crates/delta/src/lib.rs",
        ] {
            matched.extend(filters.select(path).matched());
        }
        assert!(selects(&filters, "crates/alpha/src/lib.rs"));
        assert!(selects(&filters, "crates/beta/src/lib.rs"));
        assert!(!selects(&filters, "crates/delta/src/lib.rs"));
        assert_eq!(filters.unmatched(&matched), vec!["crates/gamma"]);
    }

    /// A file may be selected by more than one fragment, and *every* fragment
    /// that selected it is reported — otherwise a redundant `--filter` would
    /// look unmatched.
    #[test]
    fn a_path_reports_every_fragment_that_matched_it() {
        let selection =
            filters(&["crates", "src", "crates/beta"]).select("crates/alpha/src/lib.rs");
        assert!(selection.is_selected());
        assert_eq!(selection.matched(), [0, 1]);
    }

    #[test]
    fn an_empty_fragment_selects_nothing_and_is_reported_unmatched() {
        let filters = filters(&[""]);
        let selection = filters.select("crates/alpha/src/lib.rs");
        assert!(!selection.is_selected());
        assert!(selection.matched().is_empty());
        assert_eq!(filters.unmatched(&BTreeSet::new()), vec![""]);
    }
}
