//! Bakes the active config profile (`config/<profile>.toml`) into the wasm.
//! Nothing network lives in code: the index principal, allowlist, budget floor,
//! prices, and per-key rate limit all come from `config/`. Principals are baked
//! as text and parsed at `init` (deploy-time, panics allowed); on `mainnet` an
//! `init` override is barred (the canister traps).

use std::{env, fs, path::Path};

fn main() {
    println!("cargo:rustc-check-cfg=cfg(crown_profile, values(\"testnet\", \"mainnet\"))");
    let profile = env::var("CROWN_PROFILE").unwrap_or_else(|_| "testnet".to_string());
    println!("cargo:rustc-cfg=crown_profile=\"{profile}\"");
    println!("cargo:rerun-if-env-changed=CROWN_PROFILE");

    let cfg_path = format!("config/{profile}.toml");
    println!("cargo:rerun-if-changed={cfg_path}");
    let text = fs::read_to_string(&cfg_path).unwrap_or_else(|_| panic!("missing {cfg_path}"));

    let index = str_of(&text, "index_principal");
    let cycle_floor = u128_of(&text, "cycle_floor");
    let ingest_price = u128_of(&text, "ingest_price");
    let sign_price = u128_of(&text, "sign_price");
    let root_price = u128_of(&text, "root_price");
    let burst_budget = u128_of(&text, "burst_budget_cycles");
    let burst_window_secs = u64::try_from(u128_of(&text, "burst_window_secs"))
        .unwrap_or_else(|_| panic!("`burst_window_secs` out of u64 range"));
    let daily_budget = u128_of(&text, "daily_budget_cycles");
    let daily_window_secs = u64::try_from(u128_of(&text, "daily_window_secs"))
        .unwrap_or_else(|_| panic!("`daily_window_secs` out of u64 range"));
    let allowlist = list_of(&text, "allowlist");
    let games = list_of(&text, "games");

    // A key must be able to afford the dearest single call, or that call is
    // permanently unreachable and the pipeline silently stalls on signatures.
    let dearest = ingest_price.max(sign_price).max(root_price);
    assert!(
        burst_budget >= dearest && daily_budget >= dearest,
        "per-key budgets (burst {burst_budget}, daily {daily_budget}) must cover the \
         dearest single call ({dearest}), else that call can never be forwarded"
    );
    // The burst window is the spike guard *inside* the day; a burst allowance
    // above the daily one would make the daily budget the only real limit.
    assert!(
        burst_budget <= daily_budget,
        "burst_budget ({burst_budget}) above daily_budget ({daily_budget}) makes the \
         burst window dead weight"
    );
    assert!(
        burst_window_secs < daily_window_secs,
        "burst window must be shorter than the daily window"
    );

    let out = format!(
        "// Baked from {cfg_path} — do not edit. Nothing network lives in code.\n\
         pub const PROFILE: &str = {profile:?};\n\
         /// Principal of crown-indexer (default; `init` may override on testnet).\n\
         pub const INDEX: &str = {index:?};\n\
         /// Allowlisted platform pushers (default; `init` may override on testnet).\n\
         pub const ALLOWLIST: &[&str] = &{allowlist:?};\n\
         /// Allowlisted game canisters — the only permitted `Game` targets\n\
         /// (default; `init` may override on testnet).\n\
         pub const GAMES: &[&str] = &{games:?};\n\
         pub const CYCLE_FLOOR: u128 = {cycle_floor};\n\
         /// Attached to an ingest forward — >= the index's INGEST_PRICE.\n\
         pub const INGEST_PRICE: u128 = {ingest_price};\n\
         /// Attached to a `RequestSignature` forward — >= the game's SIGN_PRICE.\n\
         pub const SIGN_PRICE: u128 = {sign_price};\n\
         /// Attached to a `PushRoot` forward — >= the game's ROOT_PRICE.\n\
         pub const ROOT_PRICE: u128 = {root_price};\n\
         /// Per-key cap, in **cycles** (non-negativity invariant #6): the burst\n\
         /// window bounds a spike, the daily window bounds the total a leaked key\n\
         /// can ever move. Cycles, not calls, so each call draws its real price.\n\
         pub const BURST_BUDGET_CYCLES: u128 = {burst_budget};\n\
         pub const BURST_WINDOW_SECS: u64 = {burst_window_secs};\n\
         pub const DAILY_BUDGET_CYCLES: u128 = {daily_budget};\n\
         pub const DAILY_WINDOW_SECS: u64 = {daily_window_secs};\n",
    );
    let dst = Path::new(&env::var("OUT_DIR").unwrap()).join("config.rs");
    fs::write(&dst, out).unwrap();
}

/// The raw value token of `key = <token>` (before any trailing `#` comment),
/// stripped of surrounding whitespace and one layer of quotes.
fn str_of(text: &str, key: &str) -> String {
    text.lines()
        .find_map(|l| {
            let rest = l.trim().strip_prefix(key)?.trim_start().strip_prefix('=')?;
            let rest = rest.split('#').next().unwrap_or(rest).trim();
            Some(rest.trim_matches('"').to_string())
        })
        .unwrap_or_else(|| panic!("missing `{key}` in config"))
}

/// A `u128` value, tolerating `_` digit separators.
fn u128_of(text: &str, key: &str) -> u128 {
    let raw = str_of(text, key);
    raw.replace('_', "")
        .parse()
        .unwrap_or_else(|_| panic!("`{key}` = `{raw}` is not an integer"))
}

/// A bracketed `key = ["a", "b"]` list as its quoted elements.
fn list_of(text: &str, key: &str) -> Vec<String> {
    let line = text
        .lines()
        .find(|l| l.trim_start().starts_with(key))
        .unwrap_or_else(|| panic!("missing `{key}` in config"));
    let inner = line
        .split_once('[')
        .and_then(|(_, r)| r.split_once(']'))
        .map(|(inner, _)| inner)
        .unwrap_or("");
    inner
        .split(',')
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}
