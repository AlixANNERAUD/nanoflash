//! How the free space of a segment is shared among its growing partitions.
//!
//! In-house: a few lines of integer water filling, with no crate for it.

use alloc::vec;
use alloc::vec::Vec;
use core::num::NonZeroU32;

/// What a growing partition asks of the free space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Claim {
    /// Its weight among the other claims.
    pub weight: NonZeroU32,
    /// Its share is a multiple of this power of two.
    pub unit: u64,
    /// The most it may take, a multiple of `unit`, if any.
    pub cap: Option<u64>,
}

/// `value` rounded down to a multiple of `unit` (`value` itself when `unit` is zero).
pub(crate) fn floor_to(value: u64, unit: u64) -> u64 {
    value
        .checked_rem(unit)
        .map_or(value, |rest| value.saturating_sub(rest))
}

/// Share `extra` bytes among `claims` by weight, each share rounded down to its unit and held to
/// its cap; what a cap leaves goes to the others, and the rounding remainder stays unshared.
///
/// Returns one share per claim, in order, or `None` on arithmetic overflow.
pub(crate) fn distribute(extra: u64, claims: &[Claim]) -> Option<Vec<u64>> {
    // `None` while the claim is still active.
    let mut shares: Vec<Option<u64>> = vec![None; claims.len()];
    let mut pool = extra;
    // Every round caps at least one more claim or ends the loop.
    loop {
        let total = claims
            .iter()
            .zip(&shares)
            .filter(|(_, share)| share.is_none())
            .try_fold(0u128, |total, (claim, _)| {
                total.checked_add(u128::from(claim.weight.get()))
            })?;
        if total == 0 {
            break;
        }
        let mut capped = false;
        let mut taken = 0u64;
        for (claim, share) in claims.iter().zip(shares.iter_mut()) {
            if share.is_some() {
                continue;
            }
            if let Some(cap) = claim.cap {
                let wanted = u128::from(pool).checked_mul(u128::from(claim.weight.get()))?;
                if wanted > u128::from(cap).checked_mul(total)? {
                    *share = Some(cap);
                    taken = taken.checked_add(cap)?;
                    capped = true;
                }
            }
        }
        if !capped {
            for (claim, share) in claims.iter().zip(shares.iter_mut()) {
                if share.is_none() {
                    let wanted = u128::from(pool).checked_mul(u128::from(claim.weight.get()))?;
                    let exact = u64::try_from(wanted.checked_div(total)?).ok()?;
                    *share = Some(floor_to(exact, claim.unit));
                }
            }
            break;
        }
        pool = pool.checked_sub(taken)?;
    }
    Some(shares.into_iter().map(|share| share.unwrap_or(0)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;
    use core::num::NonZeroU32;
    use proptest::prelude::*;

    fn c(weight: u32, unit: u64, cap: Option<u64>) -> Claim {
        Claim {
            weight: NonZeroU32::new(weight).unwrap(),
            unit,
            cap,
        }
    }

    #[test]
    fn shares_by_weight() {
        assert_eq!(
            distribute(0xE0000, &[c(1, 0x1000, None), c(3, 0x1000, None)]),
            Some(vec![0x38000, 0xA8000])
        );
    }

    #[test]
    fn a_cap_gives_its_leftover_to_the_others() {
        assert_eq!(
            distribute(
                0xCE0000,
                &[c(1, 0x1000, Some(0x100000)), c(3, 0x1000, None)]
            ),
            Some(vec![0x100000, 0xBE0000])
        );
    }

    #[test]
    fn caps_cascade() {
        assert_eq!(
            distribute(
                0x9000,
                &[
                    c(1, 0x1000, Some(0x1000)),
                    c(1, 0x1000, Some(0x3000)),
                    c(1, 0x1000, None)
                ]
            ),
            Some(vec![0x1000, 0x3000, 0x5000])
        );
    }

    #[test]
    fn shares_round_down_and_the_rest_stays() {
        assert_eq!(
            distribute(0xFD000, &[c(1, 0x1000, None), c(1, 0x1000, None)]),
            Some(vec![0x7E000, 0x7E000])
        );
    }

    #[test]
    fn everything_capped() {
        assert_eq!(
            distribute(
                0x100000,
                &[c(1, 0x1000, Some(0x1000)), c(1, 0x1000, Some(0x2000))]
            ),
            Some(vec![0x1000, 0x2000])
        );
    }

    #[test]
    fn a_unit_of_one_takes_every_byte() {
        assert_eq!(distribute(0xE1000, &[c(1, 1, None)]), Some(vec![0xE1000]));
    }

    #[test]
    fn edge_cases() {
        assert_eq!(distribute(0x1000, &[]), Some(vec![]));
        assert_eq!(distribute(0, &[c(1, 0x1000, Some(0))]), Some(vec![0]));
        assert_eq!(distribute(0x1000, &[c(1, 0x1000, Some(0))]), Some(vec![0]));
        assert_eq!(floor_to(0x1FFF, 0x1000), 0x1000);
        assert_eq!(floor_to(0x1FFF, 1), 0x1FFF);
        assert_eq!(floor_to(0x1FFF, 0), 0x1FFF);
        assert_eq!(floor_to(u64::MAX, 1 << 20), u64::MAX - 0xFFFFF);
    }

    fn claims() -> impl Strategy<Value = Vec<Claim>> {
        proptest::collection::vec(
            (
                1u32..=u32::MAX,
                0u32..=20,
                proptest::option::of(any::<u64>()),
            ),
            0..=64,
        )
        .prop_map(|claims| {
            claims
                .into_iter()
                .map(|(weight, shift, cap)| {
                    let unit = 1u64 << shift;
                    c(weight, unit, cap.map(|cap| floor_to(cap, unit)))
                })
                .collect()
        })
    }

    proptest! {
        #[test]
        fn distribution_is_sound(claims in claims(), extra in any::<u64>()) {
            let shares = distribute(extra, &claims);
            prop_assert!(shares.is_some());
            let shares = shares.unwrap();
            prop_assert_eq!(shares.len(), claims.len());
            let sum: u128 = shares.iter().map(|&share| u128::from(share)).sum();
            prop_assert!(sum <= u128::from(extra));
            let mut open_units = 0u128;
            let mut any_open = false;
            for (share, claim) in shares.iter().zip(&claims) {
                prop_assert_eq!(share % claim.unit, 0);
                if let Some(cap) = claim.cap {
                    prop_assert!(*share <= cap);
                }
                if claim.cap.is_none_or(|cap| *share < cap) {
                    any_open = true;
                    open_units += u128::from(claim.unit);
                }
            }
            if any_open {
                prop_assert!(u128::from(extra) - sum < open_units);
            }
        }

        #[test]
        fn distribution_is_deterministic_and_monotonic(
            claims in claims(),
            first in any::<u64>(),
            second in any::<u64>(),
        ) {
            let (small, large) = if first <= second { (first, second) } else { (second, first) };
            let shares = distribute(small, &claims).unwrap();
            prop_assert_eq!(distribute(small, &claims).unwrap(), shares.clone());
            let larger = distribute(large, &claims).unwrap();
            for (small, large) in shares.iter().zip(&larger) {
                prop_assert!(small <= large);
            }
        }
    }
}
