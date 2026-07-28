//! Baked network config (`build.rs` from `config/<profile>.toml`). Principals are
//! text here and parsed at `init`; nothing network lives in code.

include!(concat!(env!("OUT_DIR"), "/config.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn testnet_values_are_baked() {
        assert_eq!(PROFILE, "testnet");
        assert_eq!(INDEX, "aaaaa-aa");
        assert_eq!(CYCLE_FLOOR, 1_000_000_000_000);
        assert_eq!(INGEST_PRICE, 10_000_000_000);
        assert_eq!(SIGN_PRICE, 26_200_000_000);
        assert_eq!(RATE_LIMIT, 10);
        assert_eq!(RATE_WINDOW_SECS, 60);
        // The devnet allowlist is still a placeholder (real keys via `init`).
        assert_eq!(ALLOWLIST.len(), 1);
        // The games allowlist is baked (a placeholder on devnet; real via `init`).
        assert_eq!(GAMES.len(), 1);
    }

    #[test]
    fn relay_prices_are_at_least_downstream() {
        // Spec: the relay attaches at least what the index/game charge. Locals
        // (the downstream testnet prices) keep these runtime, not const, checks.
        let index_ingest_price: u128 = 10_000_000_000;
        let game_sign_price: u128 = 26_200_000_000;
        assert!(INGEST_PRICE >= index_ingest_price, "≥ index INGEST_PRICE");
        assert!(SIGN_PRICE >= game_sign_price, "≥ game SIGN_PRICE");
    }
}
