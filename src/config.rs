//! Baked network config (`build.rs` from `config/<profile>.toml`). Principals are
//! text here and parsed at `init`; nothing network lives in code.

use candid::Principal;
use std::collections::BTreeSet;

include!(concat!(env!("OUT_DIR"), "/config.rs"));

/// Whether this build is the frozen-perimeter profile, where a placeholder in the
/// config is an error rather than a value.
pub fn strict() -> bool {
    PROFILE == "mainnet"
}

/// Baked principal texts → the runtime set.
///
/// Off `mainnet` an unparseable entry (a placeholder) is dropped and the set is
/// left short: that is what lets the repo be checked out and deployed before the
/// real keys exist, and a short set only ever refuses — an empty `allowlist`
/// admits nobody, an empty `games` rejects every game call (fail closed, `00 §6`).
///
/// On `mainnet` the same entry is an `Err`, and the caller traps on it. The
/// difference is not strictness for its own sake: there the placeholder is
/// indistinguishable at runtime from a deliberately emptied list, so the deploy
/// would come up looking healthy and refuse every push — a config that was never
/// filled in, reading as a relay that simply does not work. An emptied list stays
/// expressible (it is `[]` in the config, and it parses), so the kill-switch the
/// architecture describes is untouched; what stops being expressible is the
/// unfilled one.
pub fn principal_set(entries: &[&str], strict: bool) -> Result<BTreeSet<Principal>, String> {
    let mut set = BTreeSet::new();
    for e in entries {
        match Principal::from_text(e) {
            Ok(p) => {
                set.insert(p);
            }
            Err(_) if strict => {
                return Err(format!(
                    "baked principal `{e}` is not a principal (mainnet requires real ones)"
                ))
            }
            Err(_) => {}
        }
    }
    Ok(set)
}

/// The dearest single call the relay ever forwards. Over **all** of `GameCall`
/// plus the ingest — a price left out here would be one a key cannot afford, and
/// that call would stall silently rather than fail loudly.
const DEAREST_CALL: u128 = max3(INGEST_PRICE, SIGN_PRICE, ROOT_PRICE);

const fn max3(a: u128, b: u128, c: u128) -> u128 {
    let ab = if a > b { a } else { b };
    if ab > c {
        ab
    } else {
        c
    }
}

// Non-negativity invariant #6 as a compile-time law. `build.rs` checks the same
// against the config it bakes; this checks the constants that reached the code.
const _: () = assert!(
    DAILY_BUDGET_CYCLES >= DEAREST_CALL,
    "a key must afford the dearest call, or that call is unreachable"
);
const _: () = assert!(
    BURST_BUDGET_CYCLES <= DAILY_BUDGET_CYCLES,
    "a burst allowance above the day's makes the burst window dead weight"
);
const _: () = assert!(
    BURST_WINDOW_SECS < DAILY_WINDOW_SECS,
    "the burst window is the spike guard inside the day"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn testnet_values_are_baked() {
        assert_eq!(PROFILE, "testnet");
        assert_eq!(INDEX, "aaaaa-aa");
        assert_eq!(CYCLE_FLOOR, 1_000_000_000_000);
        assert_eq!(INGEST_PRICE, 17_000_000_000);
        assert_eq!(SIGN_PRICE, 26_200_000_000);
        assert_eq!(ROOT_PRICE, 1_000_000_000);
        assert_eq!(BURST_BUDGET_CYCLES, 100_000_000_000);
        assert_eq!(BURST_WINDOW_SECS, 60);
        assert_eq!(DAILY_BUDGET_CYCLES, 10_000_000_000_000);
        assert_eq!(DAILY_WINDOW_SECS, 86_400);
        // The devnet allowlist is still a placeholder (real keys via `init`).
        assert_eq!(ALLOWLIST.len(), 1);
        // The games allowlist is baked (a placeholder on devnet; real via `init`).
        assert_eq!(GAMES.len(), 1);
        assert!(!strict(), "the testnet profile is not the frozen one");
    }

    /// A real principal parses on both profiles; the set is what the gate reads.
    #[test]
    fn real_principals_parse_on_either_profile() {
        let entries = ["aaaaa-aa", "2vxsx-fae"];
        for strict in [false, true] {
            let set = principal_set(&entries, strict).expect("real principals");
            assert_eq!(set.len(), 2);
            assert!(set.contains(&Principal::management_canister()));
            assert!(set.contains(&Principal::anonymous()));
        }
    }

    /// Off the frozen profile a placeholder is dropped, and the set is left short:
    /// short only ever refuses (an empty `allowlist` admits nobody, an empty
    /// `games` rejects every game call), which is what a repo without real keys
    /// should do.
    #[test]
    fn a_placeholder_is_dropped_off_the_frozen_profile() {
        let entries = ["PLACEHOLDER_pusher_principal", "aaaaa-aa"];
        let set = principal_set(&entries, false).expect("placeholders are values here");
        assert_eq!(
            set.len(),
            1,
            "the placeholder is dropped, the real key stays"
        );
        assert!(principal_set(&["PLACEHOLDER_x"], false)
            .expect("still Ok")
            .is_empty());
    }

    /// On the frozen profile the same placeholder is an error, and the caller
    /// traps on it. Without this the deploy comes up looking healthy and refuses
    /// every push — a config nobody filled in, reading as a relay that does not
    /// work.
    #[test]
    fn a_placeholder_is_fatal_on_the_frozen_profile() {
        let err = principal_set(&["aaaaa-aa", "PLACEHOLDER_game_mainnet_principal"], true)
            .expect_err("mainnet requires real principals");
        assert!(
            err.contains("PLACEHOLDER_game_mainnet_principal"),
            "the message must name the offending entry, got: {err}"
        );
    }

    /// An *emptied* list stays expressible on the frozen profile — it is the
    /// documented kill switch (`00 §6`: an empty `games` rejects every game call,
    /// fail closed). Only the unfilled list stops being expressible.
    #[test]
    fn an_emptied_list_is_still_legal_when_frozen() {
        assert!(principal_set(&[], true)
            .expect("empty is a value")
            .is_empty());
    }

    // **Нет теста «цены релея ≥ downstream», и это решение.** Он сравнивал
    // забейканные `INGEST_PRICE`/`SIGN_PRICE`/`ROOT_PRICE` с литералами цен
    // индекса и игр, вписанными строкой выше, — то есть с копиями, живущими в
    // этом же файле. Поднимется цена в чужом репе — литерал не сдвинется, и тест
    // останется зелёным ровно в том случае, который должен ловить. Настоящая
    // проверка требует build-зависимости на чужие конфиги, а репы независимы by
    // design (`repo-map.md`); поэтому сверка цен — шаг cost-gate
    // (`07-build-plan.md §P8`), где все они перемеряются разом. Соотношения,
    // проверяемые *внутри* этого конфига (бюджеты против `DEAREST_CALL`),
    // остаются компайл-тайм законами выше — они не вакуумны.

    /// Non-negativity invariant #6, stated as money rather than as calls. The
    /// shape of the budgets is a compile-time law (see the `const _` block); what
    /// this pins is the *magnitude* — the number a human should re-read when
    /// deciding whether the cap is still the right size.
    #[test]
    fn a_leaked_key_is_capped_by_its_day_not_by_the_relay_balance() {
        // Worst case in signatures: a leaked key cannot buy more than this many
        // threshold signatures in a day, whatever mix of calls it picks.
        let max_signs_per_day = DAILY_BUDGET_CYCLES / SIGN_PRICE;
        assert!(
            max_signs_per_day <= 1_000,
            "daily budget buys {max_signs_per_day} signatures — too loose for one key"
        );
        // And in ingests, which is what sizes normal throughput.
        let max_ingests_per_day = DAILY_BUDGET_CYCLES / INGEST_PRICE;
        assert!(
            max_ingests_per_day >= 100,
            "daily budget buys only {max_ingests_per_day} ingests — too tight to operate"
        );
    }
}
