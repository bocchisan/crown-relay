//! crown-relay — the single fronter of paid calls (canister). Ingress can't
//! attach cycles; the relay attaches them from budget (3% fees) for ingest and
//! signature requests. Not frozen — managed (allowlist, budget, index
//! principal). Powerless over the book (architecture §6).
//!
//! It fronts exactly three calls, and the set is closed by type: `index.ingest`,
//! and a game's `request_signature` / `push_root` (`GameCall`). All three are paid
//! by the game/index at the far end, and the price of each is decided here, never
//! by the caller.
//!
//! `submit` is the only non-`query`: `inspect_message` (allowlist + size) →
//! allowlist → game target → relay budget floor → per-key budgets → forward with
//! cycles. The relay's own floor is checked *before* the key is charged, so a
//! submit refused for lack of relay balance does not also consume the key's day.
//! Nothing costly happens before every gate passes. `unwrap`/`expect`/`panic` are
//! barred on the `submit` path; cycle arithmetic is `saturating_*`.

use admit::{admit, Admit, Budgets, Limits};
use candid::{CandidType, Deserialize, Encode, Principal};
use ic_cdk::call::Call;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

pub mod admit;
pub mod config;

/// Perimeter guard: the largest ingress arg accepted (a signature/request is
/// small). Oversized messages are dropped by `inspect_message`, for free.
const MAX_ARG_BYTES: usize = 8192;

thread_local! {
    static INDEX: RefCell<Principal> = const { RefCell::new(Principal::anonymous()) };
    static ALLOWLIST: RefCell<BTreeSet<Principal>> = const { RefCell::new(BTreeSet::new()) };
    /// The only canisters a `Game` call may target (budget-drain guard).
    static GAMES: RefCell<BTreeSet<Principal>> = const { RefCell::new(BTreeSet::new()) };
    static WINDOWS: RefCell<BTreeMap<Principal, Budgets>> = const { RefCell::new(BTreeMap::new()) };
}

/// The baked per-key limits, as the gate wants them (seconds → nanoseconds).
fn limits() -> Limits {
    Limits {
        burst_window_ns: config::BURST_WINDOW_SECS.saturating_mul(1_000_000_000),
        burst_budget: config::BURST_BUDGET_CYCLES,
        daily_window_ns: config::DAILY_WINDOW_SECS.saturating_mul(1_000_000_000),
        daily_budget: config::DAILY_BUDGET_CYCLES,
    }
}

/// A relayed request: one signature to fold into the book, or one paid call to a
/// game.
///
/// Ingest has a single tag, not two. `Settlement` and `Birth` were the same call
/// with the same price to the same canister — the index recognizes which the
/// transaction holds — so the distinction lived only in the caller's head, and a
/// tag that changes nothing is a tag that can be wrong.
#[derive(CandidType, Deserialize, Clone, Debug)]
pub enum Request {
    /// Fold one Solana signature — `index.ingest(signature)`.
    Ingest(String),
    /// A paid call to an allowlisted game canister.
    Game(GameReq),
}

/// A paid game call: which game, which of its paid entry points, and the
/// pre-encoded argument. The relay is a dumb proxy — it forwards `arg` verbatim
/// and never inspects the book, the verdict or the certificate.
#[derive(CandidType, Deserialize, Clone, Debug)]
pub struct GameReq {
    pub game: Principal,
    pub call: GameCall,
    pub arg: Vec<u8>,
}

/// The game entry points the relay fronts — a closed enum, deliberately not a
/// method name from the caller.
///
/// Price is a *function of the call*, so a caller can neither name a method the
/// relay has no price for nor choose which price it pays. A free-form
/// `(method, cycles)` pair would move both decisions to the caller and leave the
/// per-call ceiling to the callee's discipline — that is, to the games, which are
/// not frozen. Same reasoning as `RpcSources` in the index: the unwanted case is
/// not rejected at runtime, it is not expressible.
#[derive(CandidType, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum GameCall {
    /// `request_signature` — the paid pull of a scope's verdict signature.
    RequestSignature,
    /// `push_root` — the paid authentication of an index root (two BLS pairings,
    /// which is why it cannot live on the game's free boundary; harness §6).
    PushRoot,
}

impl GameCall {
    /// The method this call forwards to.
    fn method(self) -> &'static str {
        match self {
            GameCall::RequestSignature => "request_signature",
            GameCall::PushRoot => "push_root",
        }
    }

    /// The cycles attached to it — at least what the game charges.
    fn price(self) -> u128 {
        match self {
            GameCall::RequestSignature => config::SIGN_PRICE,
            GameCall::PushRoot => config::ROOT_PRICE,
        }
    }
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
    /// A `Game` request named a canister that is not an allowlisted game.
    UnknownGame,
    /// Over the caller's per-key burst budget — clears within the burst window.
    RateLimited,
    /// The caller spent its whole per-key daily budget. Unlike `RateLimited` this
    /// does not clear in a minute: it is the cap on what one key can ever move,
    /// and hitting it in normal operation means either the volume outgrew the
    /// config or the key is not doing what you think (invariant #6).
    KeyBudgetExhausted,
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
            let strict = config::strict();
            let index = Principal::from_text(config::INDEX)
                .unwrap_or_else(|_| ic_cdk::trap("baked index principal is invalid"));
            INDEX.with_borrow_mut(|i| *i = index);
            // Off mainnet a placeholder is dropped → a short set, which only ever
            // refuses (fail closed). On mainnet it traps instead: a lifecycle hook
            // is where a panic is a failed deploy rather than a wedged canister,
            // and an unfilled list there is otherwise indistinguishable from a
            // deliberately emptied one (`config::principal_set`).
            let set = config::principal_set(config::ALLOWLIST, strict)
                .unwrap_or_else(|e| ic_cdk::trap(&e));
            ALLOWLIST.with_borrow_mut(|a| *a = set);
            let games =
                config::principal_set(config::GAMES, strict).unwrap_or_else(|e| ic_cdk::trap(&e));
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
/// relay gates (allowlist → game target → relay budget floor → per-key budgets)
/// and forwards with the price of the call attached
/// (`INGEST_PRICE`/`SIGN_PRICE`/`ROOT_PRICE`). Dedup is the index's job, not the
/// relay's.
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

    // A `Game` call may only target an allowlisted game canister. Without this a
    // leaked or rogue allowlisted key could point the attached cycles at a canister
    // it controls and drain the budget. `Ingest` targets the trusted INDEX.
    // Gated behind `allowed` so a stranger sees `NotAllowed` (from `admit` below),
    // never probes the game set — and is rejected before consuming a rate token.
    if allowed {
        if let Request::Game(req) = &request {
            if !GAMES.with_borrow(|g| g.contains(&req.game)) {
                return SubmitResult::UnknownGame;
            }
        }
    }

    let (target, method, arg, price) = match &request {
        Request::Ingest(sig) => {
            // Refuse rather than forward what could not be encoded. The old
            // `unwrap_or_default()` sent an **empty** argument on failure: the
            // index rejects it, the call comes back `ForwardFailed`, and the key
            // has already been charged the full price for a call that was
            // malformed before it left. Encoding one `String` does not fail in
            // practice — which is the point of saying so here instead of paying
            // for the case where it does.
            let Ok(arg) = Encode!(sig) else {
                return SubmitResult::ForwardFailed;
            };
            (
                INDEX.with_borrow(|i| *i),
                "ingest",
                arg,
                config::INGEST_PRICE,
            )
        }
        Request::Game(req) => (
            req.game,
            req.call.method(),
            req.arg.clone(),
            req.call.price(),
        ),
    };

    let now = ic_cdk::api::time();
    let balance = ic_cdk::api::canister_cycle_balance();
    let verdict = WINDOWS.with_borrow_mut(|w| {
        admit(
            allowed,
            w,
            caller,
            now,
            &limits(),
            balance,
            config::CYCLE_FLOOR,
            price,
        )
    });
    match verdict {
        Admit::NotAllowed => return SubmitResult::NotAllowed,
        Admit::RateLimited => return SubmitResult::RateLimited,
        Admit::KeyBudgetExhausted => return SubmitResult::KeyBudgetExhausted,
        Admit::LowBudget => return SubmitResult::LowBudget,
        Admit::Ok => {}
    }

    // Payment leaves the budget only now, after every gate passed.
    //
    // The key is charged the full `price` whether or not the callee accepts it,
    // and the refund is deliberately **not** credited back. That looks like
    // sloppy accounting — a memoized verdict signature is served free, a duplicate
    // ingest accepts nothing, and those cycles return to this canister — but
    // crediting them reopens exactly what invariant #6 exists to close.
    //
    // The budget is denominated in cycles to bound a leaked key's *total* damage.
    // It only also bounds the *rate* because every call draws from it. Credit the
    // refunds and a leaked key gets unlimited calls for free: it submits an
    // already-applied signature over and over, is refunded every time, and each
    // round still costs this canister its own execution plus an inter-canister
    // call — a drain whose only stop would be `CYCLE_FLOOR`, i.e. the whole
    // balance. Charging for the attempt keeps both bounds; the price of that is
    // over-charging a key for work that turned out to be cheap, which costs
    // availability of *our own* key and nothing else.
    //
    // (`crown-relay/tests/e2e.rs::per_key_burst_budget_caps_a_key` is what makes
    // this real: its mock accepts no cycles, so a version that credited refunds
    // let the key run past its burst cap.)
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
