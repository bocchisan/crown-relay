//! Mock downstream for the relay e2e. Answers the two methods the relay
//! forwards to — `ingest` (index role) and `request_signature` (game role) —
//! recording which was called and how many cycles were attached, so the test
//! can assert the relay attached `INGEST_PRICE` / `SIGN_PRICE`.

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
