//! Mock downstream for the relay e2e. Answers the three methods the relay
//! forwards to — `ingest` (index role), `request_signature` and `push_root` (game
//! role) — recording which was called and how many cycles were attached, so the
//! test can assert the relay attached `INGEST_PRICE` / `SIGN_PRICE` / `ROOT_PRICE`.
//!
//! Three methods because `GameCall` has two variants and the index has one entry
//! point; a method missing here is a route the e2e cannot reach, which is exactly
//! how `push_root` stayed unrouted for as long as it did.

use std::cell::RefCell;

thread_local! {
    static CALLS: RefCell<u64> = const { RefCell::new(0) };
    static LAST_CYCLES: RefCell<u64> = const { RefCell::new(0) };
    static LAST_METHOD: RefCell<String> = const { RefCell::new(String::new()) };
}

fn record(method: &str) {
    CALLS.with(|c| *c.borrow_mut() += 1);
    LAST_CYCLES.with(|c| *c.borrow_mut() = ic_cdk::api::msg_cycles_available() as u64);
    LAST_METHOD.with(|m| *m.borrow_mut() = method.to_string());
}

/// Index role: mirrors `crown-indexer.ingest(text)`.
#[ic_cdk::update]
fn ingest(_signature: String) -> u8 {
    record("ingest");
    0
}

/// Game role: mirrors a game's `request_signature` (arg ignored).
#[ic_cdk::update(name = "request_signature")]
fn request_signature() {
    record("request_signature");
}

/// Game role: mirrors a game's `push_root(cert)` (arg ignored).
#[ic_cdk::update(name = "push_root")]
fn push_root(_cert: Vec<u8>) {
    record("push_root");
}

#[ic_cdk::query]
fn calls() -> u64 {
    CALLS.with(|c| *c.borrow())
}

#[ic_cdk::query]
fn last_cycles() -> u64 {
    LAST_CYCLES.with(|c| *c.borrow())
}

#[ic_cdk::query]
fn last_method() -> String {
    LAST_METHOD.with(|m| m.borrow().clone())
}
