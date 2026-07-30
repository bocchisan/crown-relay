//! Baked network config (`build.rs` from `config/<profile>.toml`). Principals are
//! text here and parsed at `init`; nothing network lives in code.

include!(concat!(env!("OUT_DIR"), "/config.rs"));

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
        assert_eq!(INGEST_PRICE, 13_700_000_000);
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
