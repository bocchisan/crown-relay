//! crown-relay — the single fronter of paid calls (canister). Ingress can't
//! attach cycles; the relay attaches them from budget (3% fees) for ingest and
//! signature requests. Not frozen — managed (allowlist, budget, index
//! principal). Powerless over the book (architecture §6).
//!
//! `submit` is the only non-`query`: `inspect_message` (allowlist + size) →
//! allowlist → per-key rate → budget floor → forward with cycles. Nothing
//! costly happens before every gate passes. `unwrap`/`expect`/`panic` are barred
//! on the `submit` path; cycle arithmetic is `saturating_*`.

use admit::{admit, Admit, Window};
use candid::{CandidType, Deserialize, Encode, Principal};
use ic_cdk::call::Call;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

pub mod admit;
pub mod config;

/// Perimeter guard: the largest ingress arg accepted (a signature/request is
/// small). Oversized messages are dropped by `inspect_message`, for free.
const MAX_ARG_BYTES: usize = 8192;
/// The game method a `Sign` request is forwarded to.
const SIGN_METHOD: &str = "request_signature";

thread_local! {
    static INDEX: RefCell<Principal> = const { RefCell::new(Principal::anonymous()) };
    static ALLOWLIST: RefCell<BTreeSet<Principal>> = const { RefCell::new(BTreeSet::new()) };
    /// The only canisters a `Sign` request may target (budget-drain guard).
    static GAMES: RefCell<BTreeSet<Principal>> = const { RefCell::new(BTreeSet::new()) };
    static WINDOWS: RefCell<BTreeMap<Principal, Window>> = const { RefCell::new(BTreeMap::new()) };
}

/// A relayed request: a settlement/birth signature to ingest, or a game
/// signature request. `Birth`/`Settlement` both forward to `index.ingest(sig)` —
/// the index recognizes which it is; the two tags are for the caller's clarity.
#[derive(CandidType, Deserialize, Clone, Debug)]
pub enum Request {
    Settlement(String),
    Birth(String),
    Sign(SignReq),
}

/// A game signature request: the target game and the pre-encoded call argument.
/// The relay is a dumb proxy — it forwards `request` verbatim and never inspects
/// the book or the verdict.
#[derive(CandidType, Deserialize, Clone, Debug)]
pub struct SignReq {
    pub game: Principal,
    pub request: Vec<u8>,
}

/// Deploy-time overrides (testnet only): the index principal, the pusher
/// allowlist, and the game allowlist. On mainnet an override traps — the pinned
/// config is authoritative.
#[derive(CandidType, Deserialize, Clone, Debug)]
pub struct InitArgs {
    pub index: Principal,
    pub allowlist: Vec<Principal>,
    pub games: Vec<Principal>,
}

/// Outcome of `submit`. `Forwarded` carries the downstream reply verbatim.
#[derive(CandidType, Deserialize, Clone, Debug)]
pub enum SubmitResult {
    Forwarded(Vec<u8>),
    NotAllowed,
    /// A `Sign` request named a canister that is not an allowlisted game.
    UnknownGame,
    RateLimited,
    LowBudget,
    ForwardFailed,
}

/// Configure from `init`/`post_upgrade` args (testnet) or baked config (both). On
/// mainnet an override is barred (trap); the baked, cost-gated config is
/// authoritative. All runtime state is heap-only, so this must run on **every**
/// lifecycle entry — `init` *and* `post_upgrade` — or an upgrade would silently
/// reset the allowlist/index/games to empty and brick the fronter (the relay is
/// managed, not frozen: upgrade is a normal reconfiguration point).
fn configure(overrides: Option<InitArgs>) {
    match overrides {
        Some(args) => {
            if config::PROFILE == "mainnet" {
                ic_cdk::trap("init override is barred on mainnet");
            }
            INDEX.with_borrow_mut(|i| *i = args.index);
            ALLOWLIST.with_borrow_mut(|a| *a = args.allowlist.into_iter().collect());
            GAMES.with_borrow_mut(|g| *g = args.games.into_iter().collect());
        }
        None => {
            let index = Principal::from_text(config::INDEX)
                .unwrap_or_else(|_| ic_cdk::trap("baked index principal is invalid"));
            INDEX.with_borrow_mut(|i| *i = index);
            let set = config::ALLOWLIST
                .iter()
                .filter_map(|s| Principal::from_text(s).ok())
                .collect();
            ALLOWLIST.with_borrow_mut(|a| *a = set);
            // Invalid/placeholder game principals are dropped → an empty set means
            // every `Sign` is rejected (fail closed) until real games are supplied.
            let games = config::GAMES
                .iter()
                .filter_map(|s| Principal::from_text(s).ok())
                .collect();
            GAMES.with_borrow_mut(|g| *g = games);
        }
    }
}

#[ic_cdk::init]
fn init(overrides: Option<InitArgs>) {
    configure(overrides);
}

/// Re-derive config on upgrade so a plain upgrade never wipes the heap-only state
/// (allowlist/index/games) to empty. Takes the same args as `init`: pass them to
/// reconfigure, or `null` to fall back to the baked config.
#[ic_cdk::post_upgrade]
fn post_upgrade(overrides: Option<InitArgs>) {
    configure(overrides);
}

/// Reject non-allowlisted or oversized ingress messages before execution (free).
#[ic_cdk::inspect_message]
fn inspect_message() {
    let allowed = ALLOWLIST.with_borrow(|a| a.contains(&ic_cdk::api::msg_caller()));
    let size_ok = ic_cdk::api::msg_arg_data().len() <= MAX_ARG_BYTES;
    if allowed && size_ok {
        ic_cdk::api::accept_message();
    }
}

/// The single non-`query`: an allowlisted caller submits without cycles; the
/// relay gates (allowlist → game target → per-key rate → budget floor) and forwards
/// with `INGEST_PRICE`/`SIGN_PRICE` attached. Dedup is the index's job, not the relay's.
#[ic_cdk::update]
async fn submit(request: Request) -> SubmitResult {
    let caller = ic_cdk::api::msg_caller();
    // Perimeter size bound, re-asserted on the replicated path. `inspect_message`
    // drops oversized *ingress* for free, but inter-canister calls skip it, so the
    // 8 KiB bound is re-checked here too (defense in depth — the forwarded request
    // stays small even from a canister caller).
    if ic_cdk::api::msg_arg_data().len() > MAX_ARG_BYTES {
        return SubmitResult::ForwardFailed;
    }
    let allowed = ALLOWLIST.with_borrow(|a| a.contains(&caller));

    // A `Sign` may only target an allowlisted game canister. Without this a leaked
    // or rogue allowlisted key could point the attached cycles at a canister it
    // controls and drain the budget. `Settlement`/`Birth` target the trusted INDEX.
    // Gated behind `allowed` so a stranger sees `NotAllowed` (from `admit` below),
    // never probes the game set — and is rejected before consuming a rate token.
    if allowed {
        if let Request::Sign(req) = &request {
            if !GAMES.with_borrow(|g| g.contains(&req.game)) {
                return SubmitResult::UnknownGame;
            }
        }
    }

    let (target, method, arg, price) = match &request {
        Request::Settlement(sig) | Request::Birth(sig) => (
            INDEX.with_borrow(|i| *i),
            "ingest",
            Encode!(sig).unwrap_or_default(),
            config::INGEST_PRICE,
        ),
        Request::Sign(req) => (
            req.game,
            SIGN_METHOD,
            req.request.clone(),
            config::SIGN_PRICE,
        ),
    };

    let now = ic_cdk::api::time();
    let balance = ic_cdk::api::canister_cycle_balance();
    let window_ns = config::RATE_WINDOW_SECS.saturating_mul(1_000_000_000);
    let verdict = WINDOWS.with_borrow_mut(|w| {
        admit(
            allowed,
            w,
            caller,
            now,
            window_ns,
            config::RATE_LIMIT,
            balance,
            config::CYCLE_FLOOR,
            price,
        )
    });
    match verdict {
        Admit::NotAllowed => return SubmitResult::NotAllowed,
        Admit::RateLimited => return SubmitResult::RateLimited,
        Admit::LowBudget => return SubmitResult::LowBudget,
        Admit::Ok => {}
    }

    // Payment leaves the budget only now, after every gate passed.
    match Call::unbounded_wait(target, method)
        .with_raw_args(&arg)
        .with_cycles(price)
        .await
    {
        Ok(reply) => SubmitResult::Forwarded(reply.into_bytes()),
        Err(_) => SubmitResult::ForwardFailed,
    }
}

/// Principal of crown-indexer. Free query.
#[ic_cdk::query]
fn get_index() -> Principal {
    INDEX.with_borrow(|i| *i)
}

ic_cdk::export_candid!();
