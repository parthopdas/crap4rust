//! Pure, I/O-free CRAP metric domain.
//!
//! Computes the CRAP score from a function's cyclomatic complexity and its
//! coverage fraction, and classifies a score into a fixed display band (C11).
//!
//! **CRAP** = `cc² · (1 − cov)³ + cc`, where `cc` is cyclomatic complexity and
//! `cov` is a coverage **fraction in `[0.0, 1.0]`** (not a percentage). All
//! arithmetic is done in `f64`; `cc` is cast once.
//!
//! Coverage may be **absent** at the domain boundary (file not in the LCOV
//! profile ⇒ `N/A`, unscored — C13). That is modelled as `Option<f64>`: the
//! [`score`] wrapper returns `None` for absent coverage (sorts last in the
//! reporter) and `Some(crap(cc, f))` otherwise. The join that produces `cov`
//! (including the `total=0 ⇒ cov=1` ruling of C13) is T5's job — this module
//! only consumes the resulting fraction.
//!
//! Bands are **report-only** and independent of any `--threshold` gate (C11):
//! classification here never affects exit codes (C6 parity — reporter, not gate).
//!
//! Consumed by the coverage join (T5) and reporter (T6); until the pipeline is
//! wired it is exercised only by unit tests, so `dead_code` is allowed here.
//! **Remove this `allow` on T5/T7** once the pipeline uses the module.
#![allow(dead_code)]

/// Fixed display bands for a CRAP score (C11). Report-only; independent of any
/// threshold gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RiskBand {
    /// CRAP ≤ 5.0.
    Low,
    /// 5.0 < CRAP ≤ 30.0.
    Moderate,
    /// CRAP > 30.0.
    High,
}

/// The CRAP score: `cc² · (1 − cov)³ + cc`.
///
/// `cov` is a coverage fraction in `[0.0, 1.0]` (not a percentage). The
/// complexity `cc` is cast to `f64` once and all arithmetic is done in `f64`.
pub(crate) fn crap(cc: u32, cov: f64) -> f64 {
    let cc = f64::from(cc);
    let uncovered = 1.0 - cov;
    cc * cc * uncovered * uncovered * uncovered + cc
}

/// Score a function, propagating absent coverage.
///
/// `cov == None` (file absent from the profile ⇒ N/A, unscored) yields `None`;
/// otherwise the CRAP score is `Some(crap(cc, f))`.
pub(crate) fn score(cc: u32, cov: Option<f64>) -> Option<f64> {
    cov.map(|f| crap(cc, f))
}

/// Classify a CRAP score into a fixed display band (C11).
///
/// Boundaries are at 5.0 and 30.0; both are **lower-inclusive on the safer
/// band**: `crap ≤ 5.0 → Low`, `5.0 < crap ≤ 30.0 → Moderate`,
/// `crap > 30.0 → High`. So exactly `5.0` is `Low` and exactly `30.0` is
/// `Moderate`.
pub(crate) fn band(crap: f64) -> RiskBand {
    if crap <= 5.0 {
        RiskBand::Low
    } else if crap <= 30.0 {
        RiskBand::Moderate
    } else {
        RiskBand::High
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPSILON: f64 = 1e-9;

    #[test]
    fn crap_known_vectors() {
        // Integer-clean vectors — exact equality.
        assert_eq!(crap(1, 1.0), 1.0);
        assert_eq!(crap(5, 0.0), 30.0); // 25·1 + 5
        assert_eq!(crap(5, 1.0), 5.0);
        // Fractional vector — epsilon comparison: 4·0.125 + 2 = 2.5.
        assert!((crap(2, 0.5) - 2.5).abs() < EPSILON);
    }

    #[test]
    fn score_propagates_absent_coverage() {
        assert_eq!(score(7, None), None);
    }

    #[test]
    fn score_matches_crap_when_present() {
        for &(cc, cov) in &[(1u32, 1.0f64), (5, 0.0), (5, 1.0), (2, 0.5), (10, 0.73)] {
            assert_eq!(score(cc, Some(cov)), Some(crap(cc, cov)));
        }
    }

    #[test]
    fn band_boundaries() {
        struct Case {
            crap: f64,
            expected: RiskBand,
        }

        let cases = [
            Case {
                crap: 4.9,
                expected: RiskBand::Low,
            },
            Case {
                crap: 5.0,
                expected: RiskBand::Low,
            },
            Case {
                crap: 5.1,
                expected: RiskBand::Moderate,
            },
            Case {
                crap: 29.9,
                expected: RiskBand::Moderate,
            },
            Case {
                crap: 30.0,
                expected: RiskBand::Moderate,
            },
            Case {
                crap: 30.1,
                expected: RiskBand::High,
            },
        ];

        for c in &cases {
            assert_eq!(band(c.crap), c.expected, "band({})", c.crap);
        }
    }

    #[test]
    fn band_representative_mid_values() {
        assert_eq!(band(crap(5, 1.0)), RiskBand::Low); // 5.0
        assert_eq!(band(crap(2, 0.5)), RiskBand::Low); // 2.5
        assert_eq!(band(crap(5, 0.0)), RiskBand::Moderate); // 30.0
        assert_eq!(band(15.0), RiskBand::Moderate);
        assert_eq!(band(crap(10, 0.0)), RiskBand::High); // 110.0
    }
}
