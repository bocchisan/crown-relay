//! The relay's admission gate: the fixed order of cheap checks before any spend
//! (spec §Поверхность). A caller that isn't allowlisted, is over its per-key
//! window, or would breach the budget floor is rejected before any cycles are
//! attached or forward is made. Non-negativity invariant #6 (per-key cap): a
//! leaked key degrades — it cannot drain the budget within a window.

use candid::Principal;
use std::collections::BTreeMap;

/// A per-key fixed window: when it started and how many submits it has seen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Window {
    pub start: u64,
    pub count: u32,
}

/// The gate's verdict. Only `Ok` may attach cycles and forward.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admit {
    Ok,
    /// Caller is not on the allowlist (also caught earlier by `inspect_message`).
    NotAllowed,
    /// Caller exceeded its per-key window (invariant #6).
    RateLimited,
    /// Forwarding the price would dip below the budget floor.
    LowBudget,
}

/// Whether the budget can afford `price` above the floor (saturating add so a
/// huge price can never wrap under the floor).
pub fn budget_ok(balance: u128, floor: u128, price: u128) -> bool {
    balance >= floor.saturating_add(price)
}

/// Record one submit against `key`'s window and say whether it stays within the
/// cap. The window rolls once `now` is `window_ns` past its start.
pub fn rate_ok(
    windows: &mut BTreeMap<Principal, Window>,
    key: Principal,
    now: u64,
    window_ns: u64,
    limit: u32,
) -> bool {
    let w = windows.entry(key).or_default();
    if now.saturating_sub(w.start) >= window_ns {
        w.start = now;
        w.count = 0;
    }
    if w.count >= limit {
        return false;
    }
    w.count = w.count.saturating_add(1);
    true
}

/// The full decision, in spec order: allowlist → per-key rate → budget floor.
/// Allowlist is checked first and consumes no rate token (a stranger never taxes
/// an allowlisted key's window).
#[allow(clippy::too_many_arguments)]
pub fn admit(
    allowlisted: bool,
    windows: &mut BTreeMap<Principal, Window>,
    key: Principal,
    now: u64,
    window_ns: u64,
    limit: u32,
    balance: u128,
    floor: u128,
    price: u128,
) -> Admit {
    if !allowlisted {
        return Admit::NotAllowed;
    }
    if !rate_ok(windows, key, now, window_ns, limit) {
        return Admit::RateLimited;
    }
    if !budget_ok(balance, floor, price) {
        return Admit::LowBudget;
    }
    Admit::Ok
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLOOR: u128 = 1_000_000_000_000;
    const PRICE: u128 = 10_000_000_000;
    const WINDOW: u64 = 60_000_000_000; // 60s in ns
    const LIMIT: u32 = 3;

    fn key(b: u8) -> Principal {
        Principal::from_slice(&[b; 10])
    }

    #[test]
    fn budget_floor_is_respected() {
        assert!(budget_ok(FLOOR + PRICE, FLOOR, PRICE));
        assert!(!budget_ok(FLOOR + PRICE - 1, FLOOR, PRICE));
        // A price near u128::MAX cannot wrap the floor under it.
        assert!(!budget_ok(FLOOR, FLOOR, u128::MAX));
    }

    #[test]
    fn per_key_window_caps_then_rolls() {
        let mut w = BTreeMap::new();
        let k = key(1);
        for _ in 0..LIMIT {
            assert!(rate_ok(&mut w, k, 0, WINDOW, LIMIT));
        }
        // The next submit in the same window is capped.
        assert!(!rate_ok(&mut w, k, 10, WINDOW, LIMIT));
        // A different key has its own budget.
        assert!(rate_ok(&mut w, key(2), 10, WINDOW, LIMIT));
        // Once the window rolls, the original key is admitted again.
        assert!(rate_ok(&mut w, k, WINDOW, WINDOW, LIMIT));
    }

    #[test]
    fn admit_orders_allowlist_then_rate_then_budget() {
        let mut w = BTreeMap::new();
        let k = key(1);

        // Not allowlisted → rejected without consuming a rate token.
        assert_eq!(
            admit(
                false,
                &mut w,
                k,
                0,
                WINDOW,
                LIMIT,
                FLOOR + PRICE,
                FLOOR,
                PRICE
            ),
            Admit::NotAllowed
        );
        assert!(w.is_empty(), "a stranger must not touch the window map");

        // Allowlisted + funded → Ok up to the cap.
        for _ in 0..LIMIT {
            assert_eq!(
                admit(
                    true,
                    &mut w,
                    k,
                    0,
                    WINDOW,
                    LIMIT,
                    FLOOR + PRICE,
                    FLOOR,
                    PRICE
                ),
                Admit::Ok
            );
        }
        // Over the cap → RateLimited (budget still fine).
        assert_eq!(
            admit(
                true,
                &mut w,
                k,
                0,
                WINDOW,
                LIMIT,
                FLOOR + PRICE,
                FLOOR,
                PRICE
            ),
            Admit::RateLimited
        );

        // Fresh key, funded below floor+price → LowBudget.
        assert_eq!(
            admit(true, &mut w, key(9), 0, WINDOW, LIMIT, FLOOR, FLOOR, PRICE),
            Admit::LowBudget
        );
    }
}
