//! The relay's admission gate: the fixed order of cheap checks before any spend
//! (spec §Поверхность). A caller that isn't allowlisted, is over either of its
//! per-key budgets, or would breach the budget floor is rejected before any
//! cycles are attached or forward is made.
//!
//! Non-negativity invariant #6 (per-key cap). The budgets are denominated in
//! **cycles, not calls**, for two reasons. A call cap bounds the *rate* of a
//! leaked key but not the *total* — the only real stop was `CYCLE_FLOOR`, i.e.
//! the whole balance — so a slow leak drained everything and merely took longer.
//! And a call cap prices a `Sign` (`SIGN_PRICE`) the same as an ingest, though it
//! costs ~2.6× more. Charging the actual price against a short **burst** window
//! and a long **daily** window bounds both the spike and the day: the most a
//! leaked key can ever move is `daily_budget`, whatever mix of calls it picks.

use candid::Principal;
use std::collections::BTreeMap;

/// A per-key fixed window: when it started and how many cycles it has spent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Window {
    pub start: u64,
    pub spent: u128,
}

/// Both windows a key is charged against: a short burst window and a long daily
/// one. The burst window caps a spike, the daily window caps the damage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Budgets {
    pub burst: Window,
    pub daily: Window,
}

/// The gate's verdict. Only `Ok` may attach cycles and forward.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admit {
    Ok,
    /// Caller is not on the allowlist (also caught earlier by `inspect_message`).
    NotAllowed,
    /// Caller exceeded its per-key burst budget (invariant #6).
    RateLimited,
    /// Caller exhausted its per-key daily budget (invariant #6). Distinct from
    /// `RateLimited`: a burst clears in a minute, this one means the key has
    /// spent its day and something is wrong.
    KeyBudgetExhausted,
    /// Forwarding the price would dip below the relay's own budget floor.
    LowBudget,
}

/// Whether the budget can afford `price` above the floor (saturating add so a
/// huge price can never wrap under the floor).
pub fn budget_ok(balance: u128, floor: u128, price: u128) -> bool {
    balance >= floor.saturating_add(price)
}

/// Whether `price` fits in `w`'s remaining allowance, charging it if so. The
/// window rolls once `now` is `window_ns` past its start. Arithmetic saturates:
/// an absurd price can never wrap the counter back under the cap.
fn charge(w: &mut Window, now: u64, window_ns: u64, cap: u128, price: u128) -> bool {
    if now.saturating_sub(w.start) >= window_ns {
        w.start = now;
        w.spent = 0;
    }
    let after = w.spent.saturating_add(price);
    if after > cap {
        return false;
    }
    w.spent = after;
    true
}

/// The relay's per-key limits (from baked config), grouped so the gate keeps a
/// readable signature as it gained a second window.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub burst_window_ns: u64,
    pub burst_budget: u128,
    pub daily_window_ns: u64,
    pub daily_budget: u128,
}

/// Charge `price` against `key`'s burst and daily budgets. Both must have room;
/// neither is charged unless both do, so a rejected submit costs the key nothing.
pub fn rate_ok(
    budgets: &mut BTreeMap<Principal, Budgets>,
    key: Principal,
    now: u64,
    limits: &Limits,
    price: u128,
) -> Admit {
    let b = budgets.entry(key).or_default();
    // Probe on copies first: charging the burst window and then failing the daily
    // one would bill a key for a submit that never happened.
    let mut burst = b.burst;
    let mut daily = b.daily;
    if !charge(
        &mut burst,
        now,
        limits.burst_window_ns,
        limits.burst_budget,
        price,
    ) {
        return Admit::RateLimited;
    }
    if !charge(
        &mut daily,
        now,
        limits.daily_window_ns,
        limits.daily_budget,
        price,
    ) {
        return Admit::KeyBudgetExhausted;
    }
    b.burst = burst;
    b.daily = daily;
    Admit::Ok
}

/// The full decision, in spec order: allowlist → per-key budgets → relay floor.
/// Allowlist is checked first and charges nothing (a stranger never taxes an
/// allowlisted key's budget).
#[allow(clippy::too_many_arguments)]
pub fn admit(
    allowlisted: bool,
    budgets: &mut BTreeMap<Principal, Budgets>,
    key: Principal,
    now: u64,
    limits: &Limits,
    balance: u128,
    floor: u128,
    price: u128,
) -> Admit {
    if !allowlisted {
        return Admit::NotAllowed;
    }
    // The relay's own floor is checked before the key is charged, so a submit
    // refused for lack of relay balance does not also consume the key's day.
    if !budget_ok(balance, floor, price) {
        return Admit::LowBudget;
    }
    rate_ok(budgets, key, now, limits, price)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLOOR: u128 = 1_000_000_000_000;
    const PRICE: u128 = 10_000_000_000;
    const SIGN: u128 = 26_000_000_000;
    const BURST_WINDOW: u64 = 60_000_000_000; // 60s in ns
    const DAY: u64 = 86_400_000_000_000; // 24h in ns

    /// Three ingests per burst, ten per day — small enough to hit both edges.
    fn limits() -> Limits {
        Limits {
            burst_window_ns: BURST_WINDOW,
            burst_budget: PRICE * 3,
            daily_window_ns: DAY,
            daily_budget: PRICE * 10,
        }
    }

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
    fn burst_window_caps_then_rolls() {
        let mut b = BTreeMap::new();
        let (k, l) = (key(1), limits());
        for _ in 0..3 {
            assert_eq!(rate_ok(&mut b, k, 0, &l, PRICE), Admit::Ok);
        }
        // The next submit in the same burst window is capped.
        assert_eq!(rate_ok(&mut b, k, 10, &l, PRICE), Admit::RateLimited);
        // A different key has its own budgets.
        assert_eq!(rate_ok(&mut b, key(2), 10, &l, PRICE), Admit::Ok);
        // Once the burst window rolls, the original key is admitted again.
        assert_eq!(rate_ok(&mut b, k, BURST_WINDOW, &l, PRICE), Admit::Ok);
    }

    /// The point of the daily window: a key that paces itself under the burst cap
    /// still cannot spend more than its day. This is what a call-rate limit alone
    /// could not do — it bounded the spike, never the total.
    #[test]
    fn a_paced_key_still_runs_out_of_day() {
        let mut b = BTreeMap::new();
        let (k, l) = (key(1), limits());
        // One submit per burst window, always under the burst cap.
        for i in 0..10u64 {
            let now = i.saturating_mul(BURST_WINDOW);
            assert_eq!(rate_ok(&mut b, k, now, &l, PRICE), Admit::Ok, "submit {i}");
        }
        // The 11th is refused by the daily budget, not the burst one.
        let now = 10u64.saturating_mul(BURST_WINDOW);
        assert_eq!(
            rate_ok(&mut b, k, now, &l, PRICE),
            Admit::KeyBudgetExhausted
        );
        // It clears only when the day rolls.
        assert_eq!(rate_ok(&mut b, k, DAY, &l, PRICE), Admit::Ok);
    }

    /// Budgets are in cycles, so a `Sign` costs what a `Sign` costs — a key
    /// cannot get 2.6× more value out of its day by picking the pricier call.
    #[test]
    fn a_sign_draws_its_real_price_not_one_token() {
        let mut b = BTreeMap::new();
        let l = Limits {
            burst_budget: SIGN * 10, // burst out of the way; test the day
            ..limits()
        };
        let k = key(1);
        // daily_budget = 10·PRICE = 100e9 → three signs (78e9) fit, a fourth does not.
        for i in 0..3 {
            assert_eq!(rate_ok(&mut b, k, 0, &l, SIGN), Admit::Ok, "sign {i}");
        }
        assert_eq!(rate_ok(&mut b, k, 0, &l, SIGN), Admit::KeyBudgetExhausted);
        // The remaining 22e9 is still spendable on cheaper calls.
        assert_eq!(rate_ok(&mut b, k, 0, &l, PRICE), Admit::Ok);
    }

    /// A submit refused by the daily budget must not have already been billed to
    /// the burst window — otherwise a key rejected all day would also lose its
    /// burst allowance for calls it never made.
    #[test]
    fn a_rejected_submit_charges_neither_window() {
        let mut b = BTreeMap::new();
        let (k, l) = (key(1), limits());
        // Spend the whole day, paced one submit per burst window so the burst cap
        // is never the binding constraint.
        for i in 0..10u64 {
            let now = i.saturating_mul(BURST_WINDOW);
            assert_eq!(rate_ok(&mut b, k, now, &l, PRICE), Admit::Ok, "submit {i}");
        }
        let now = 10u64.saturating_mul(BURST_WINDOW);
        let before = b[&k];
        assert_eq!(
            rate_ok(&mut b, k, now, &l, PRICE),
            Admit::KeyBudgetExhausted
        );
        assert_eq!(b[&k], before, "a refused submit must cost the key nothing");
    }

    #[test]
    fn admit_orders_allowlist_then_relay_floor_then_key_budgets() {
        let mut b = BTreeMap::new();
        let (k, l) = (key(1), limits());
        let funded = FLOOR + PRICE;

        // Not allowlisted → rejected without touching any budget.
        assert_eq!(
            admit(false, &mut b, k, 0, &l, funded, FLOOR, PRICE),
            Admit::NotAllowed
        );
        assert!(b.is_empty(), "a stranger must not touch the budget map");

        // Relay below floor+price → LowBudget, and the key is not charged for a
        // submit the relay could not afford to make.
        assert_eq!(
            admit(true, &mut b, k, 0, &l, FLOOR, FLOOR, PRICE),
            Admit::LowBudget
        );
        assert!(b.is_empty(), "LowBudget must not consume the key's day");

        // Allowlisted + funded → Ok up to the burst cap, then RateLimited.
        for _ in 0..3 {
            assert_eq!(
                admit(true, &mut b, k, 0, &l, funded, FLOOR, PRICE),
                Admit::Ok
            );
        }
        assert_eq!(
            admit(true, &mut b, k, 0, &l, funded, FLOOR, PRICE),
            Admit::RateLimited
        );
    }
}
