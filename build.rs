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
    let rate_limit = u32::try_from(u128_of(&text, "rate_limit"))
        .unwrap_or_else(|_| panic!("`rate_limit` out of u32 range"));
    let rate_window_secs = u64::try_from(u128_of(&text, "rate_window_secs"))
        .unwrap_or_else(|_| panic!("`rate_window_secs` out of u64 range"));
    let allowlist = list_of(&text, "allowlist");
    let games = list_of(&text, "games");

    let out = format!(
        "// Baked from {cfg_path} — do not edit. Nothing network lives in code.\n\
         pub const PROFILE: &str = {profile:?};\n\
         /// Principal of crown-indexer (default; `init` may override on testnet).\n\
         pub const INDEX: &str = {index:?};\n\
         /// Allowlisted platform pushers (default; `init` may override on testnet).\n\
         pub const ALLOWLIST: &[&str] = &{allowlist:?};\n\
         /// Allowlisted game canisters — the only permitted `Sign` targets\n\
         /// (default; `init` may override on testnet).\n\
         pub const GAMES: &[&str] = &{games:?};\n\
         pub const CYCLE_FLOOR: u128 = {cycle_floor};\n\
         /// Attached to an ingest forward — >= the index's INGEST_PRICE.\n\
         pub const INGEST_PRICE: u128 = {ingest_price};\n\
         /// Attached to a signature forward — >= the game's SIGN_PRICE.\n\
         pub const SIGN_PRICE: u128 = {sign_price};\n\
         /// Per-key cap: max submits per window (non-negativity invariant #6).\n\
         pub const RATE_LIMIT: u32 = {rate_limit};\n\
         pub const RATE_WINDOW_SECS: u64 = {rate_window_secs};\n",
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
