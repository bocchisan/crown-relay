//! PocketIC end-to-end for the relay on a real IC replica.
//!
//! Run with the bundled server:
//!   POCKET_IC_BIN=~/.cache/dfinity/versions/<v>/pocket-ic cargo test --test e2e

use candid::{Decode, Encode, Principal};
use crown_relay::config;
use crown_relay::{GameCall, GameReq, InitArgs, Request, SubmitResult};
use pocket_ic::{PocketIc, PocketIcBuilder};

const T_CYCLES: u128 = 4_000_000_000_000;

/// A fixed allowlisted pusher principal for the tests.
fn pusher() -> Principal {
    Principal::from_slice(&[42u8; 10])
}

/// Build a canister wasm and read back **the file that build just wrote**.
///
/// `--target-dir` is resolved against the process's working directory, so it and
/// the read path must be rooted the same way. They were not: the mock built with
/// `current_dir("e2e-mock")` and `--target-dir e2e-mock/target` landed in
/// `e2e-mock/e2e-mock/target/`, while the read took `e2e-mock/target/` — whatever
/// stale wasm an earlier, correct build had left there. The suite then ran green
/// against a mock that no longer matched its source, which is the worst way for
/// this to fail: a change to the mock simply had no effect. `P7.6` deleted the
/// stray directory but not the cause, so it came back on the next run.
fn build_wasm(target_dir: &str, package_dir: Option<&str>, profile: &str) -> Vec<u8> {
    let (crate_name, cwd) = match package_dir {
        Some(p) => ("mock_downstream", p),
        None => ("crown_relay", "."),
    };
    let mut cmd = std::process::Command::new("cargo");
    cmd.args([
        "build",
        "--lib",
        "--release",
        "--target",
        "wasm32-unknown-unknown",
        "--target-dir",
        target_dir,
    ]);
    cmd.env("CROWN_PROFILE", profile);
    cmd.current_dir(cwd);
    assert!(cmd.status().expect("cargo build").success(), "build failed");
    // Same root as `current_dir`, so this can only ever read what was just built.
    let out = format!("{cwd}/{target_dir}/wasm32-unknown-unknown/release/{crate_name}.wasm");
    std::fs::read(&out).unwrap_or_else(|_| panic!("read {out}"))
}

fn relay_wasm() -> Vec<u8> {
    build_wasm("target", None, "testnet")
}

fn mock_wasm() -> Vec<u8> {
    build_wasm("target", Some("e2e-mock"), "testnet")
}

/// Relay (allowlisting `pusher`, index = a fresh mock) + the mock, both on an
/// application subnet. Returns `(pic, relay, mock)`.
fn setup() -> (PocketIc, Principal, Principal) {
    let pic = PocketIcBuilder::new()
        .with_nns_subnet()
        .with_application_subnet()
        .build();
    let app = pic.topology().get_app_subnets()[0];

    let mock = pic.create_canister_on_subnet(None, None, app);
    pic.add_cycles(mock, T_CYCLES);
    pic.install_canister(mock, mock_wasm(), Encode!().unwrap(), None);

    let relay = pic.create_canister_on_subnet(None, None, app);
    pic.add_cycles(relay, T_CYCLES);
    let init = InitArgs {
        index: mock,
        allowlist: vec![pusher()],
        games: vec![mock], // the mock also serves the game role
    };
    pic.install_canister(relay, relay_wasm(), Encode!(&Some(init)).unwrap(), None);
    (pic, relay, mock)
}

fn submit(pic: &PocketIc, relay: Principal, sender: Principal, req: &Request) -> SubmitResult {
    let bytes = pic
        .update_call(relay, sender, "submit", Encode!(req).unwrap())
        .expect("submit rejected");
    Decode!(&bytes, SubmitResult).unwrap()
}

fn q_u64(pic: &PocketIc, id: Principal, method: &str) -> u64 {
    Decode!(
        &pic.query_call(id, Principal::anonymous(), method, Encode!().unwrap())
            .unwrap(),
        u64
    )
    .unwrap()
}

fn q_string(pic: &PocketIc, id: Principal, method: &str) -> String {
    Decode!(
        &pic.query_call(id, Principal::anonymous(), method, Encode!().unwrap())
            .unwrap(),
        String
    )
    .unwrap()
}

#[test]
fn non_allowlisted_submit_is_rejected_by_inspect_message() {
    let (pic, relay, mock) = setup();
    let stranger = Principal::from_slice(&[7u8; 10]);
    // inspect_message must drop it before execution → the ingress is rejected.
    let res = pic.update_call(
        relay,
        stranger,
        "submit",
        Encode!(&Request::Ingest("sig".to_string())).unwrap(),
    );
    assert!(res.is_err(), "a stranger must be rejected, not executed");
    assert_eq!(q_u64(&pic, mock, "calls"), 0, "no forward happened");
}

#[test]
fn allowlisted_settlement_forwards_to_index_with_ingest_price() {
    let (pic, relay, mock) = setup();
    let res = submit(&pic, relay, pusher(), &Request::Ingest("sig-1".to_string()));
    assert!(matches!(res, SubmitResult::Forwarded(_)), "got {res:?}");
    assert_eq!(q_u64(&pic, mock, "calls"), 1);
    assert_eq!(q_string(&pic, mock, "last_method"), "ingest");
    assert_eq!(
        q_u64(&pic, mock, "last_cycles"),
        config::INGEST_PRICE as u64
    );
}

#[test]
fn sign_forwards_to_the_game_with_sign_price() {
    let (pic, relay, mock) = setup();
    let req = Request::Game(GameReq {
        game: mock, // the mock also serves the game role
        call: GameCall::RequestSignature,
        arg: Encode!().unwrap(),
    });
    let res = submit(&pic, relay, pusher(), &req);
    assert!(matches!(res, SubmitResult::Forwarded(_)), "got {res:?}");
    assert_eq!(q_string(&pic, mock, "last_method"), "request_signature");
    assert_eq!(q_u64(&pic, mock, "last_cycles"), config::SIGN_PRICE as u64);
}

/// `push_root` is the other paid game call, and it must be reachable *here*.
///
/// The games require `ROOT_PRICE` before authenticating an index root, and ingress
/// cannot attach cycles — so without this route the documented client preamble
/// (push the root, then prove against it) had no paid path at all, and the only
/// way to run it was an operator canister outside the allowlist and outside the
/// per-key budget, i.e. outside invariant #6.
#[test]
fn push_root_forwards_to_the_game_with_root_price() {
    let (pic, relay, mock) = setup();
    let req = Request::Game(GameReq {
        game: mock,
        call: GameCall::PushRoot,
        arg: Encode!(&vec![7u8; 32]).unwrap(),
    });
    let res = submit(&pic, relay, pusher(), &req);
    assert!(matches!(res, SubmitResult::Forwarded(_)), "got {res:?}");
    assert_eq!(q_string(&pic, mock, "last_method"), "push_root");
    // Its own price, not the signature's — the cycles a call draws are a function
    // of the call. (Today that is ~26× cheaper; the property under test is the
    // mapping, not the ratio, so no assertion is made on the numbers.)
    assert_eq!(q_u64(&pic, mock, "last_cycles"), config::ROOT_PRICE as u64);
}

/// The game allowlist guards *every* game call, not just the signature one —
/// otherwise the cheaper route would be the open one.
#[test]
fn push_root_to_an_unknown_game_is_rejected_without_forwarding() {
    let (pic, relay, mock) = setup(); // games = [mock]
    let rogue = Principal::from_slice(&[9u8; 10]);
    let req = Request::Game(GameReq {
        game: rogue,
        call: GameCall::PushRoot,
        arg: Encode!(&vec![7u8; 32]).unwrap(),
    });
    let res = submit(&pic, relay, pusher(), &req);
    assert!(matches!(res, SubmitResult::UnknownGame), "got {res:?}");
    assert_eq!(q_u64(&pic, mock, "calls"), 0);
}

#[test]
fn sign_to_an_unknown_game_is_rejected_without_forwarding() {
    let (pic, relay, mock) = setup(); // games = [mock]
    let rogue = Principal::from_slice(&[9u8; 10]); // not an allowlisted game
    let req = Request::Game(GameReq {
        game: rogue,
        call: GameCall::RequestSignature,
        arg: Encode!().unwrap(),
    });
    let res = submit(&pic, relay, pusher(), &req);
    assert!(matches!(res, SubmitResult::UnknownGame), "got {res:?}");
    // No cycles left the budget — the drain is blocked before the forward.
    assert_eq!(
        q_u64(&pic, mock, "calls"),
        0,
        "no forward to an unknown game"
    );
}

/// The per-key cap is denominated in cycles (invariant #6), so the number of
/// ingests a key gets in one burst window is its burst budget divided by the
/// ingest price — and the very next submit is refused before any forward.
#[test]
fn per_key_burst_budget_caps_a_key() {
    let (pic, relay, mock) = setup();
    let affordable = config::BURST_BUDGET_CYCLES / config::INGEST_PRICE;
    assert!(affordable >= 1, "a key must afford at least one ingest");
    for i in 0..affordable {
        let res = submit(&pic, relay, pusher(), &Request::Ingest("s".to_string()));
        assert!(matches!(res, SubmitResult::Forwarded(_)), "submit {i}");
    }
    // The next submit in the window is capped — no further forward.
    let capped = submit(&pic, relay, pusher(), &Request::Ingest("s".to_string()));
    assert!(
        matches!(capped, SubmitResult::RateLimited),
        "got {capped:?}"
    );
    assert_eq!(q_u64(&pic, mock, "calls"), affordable as u64);
}

/// A `Sign` draws `SIGN_PRICE`, not one token: the same burst budget buys
/// strictly fewer signatures than ingests, because a signature costs more.
#[test]
fn a_sign_draws_more_of_the_key_budget_than_an_ingest() {
    // Presumes a signature is the dearer call (it is, in every profile).
    let signs = config::BURST_BUDGET_CYCLES / config::SIGN_PRICE;
    let ingests = config::BURST_BUDGET_CYCLES / config::INGEST_PRICE;
    assert!(
        signs < ingests,
        "a cycle-denominated cap must price a Sign above an ingest ({signs} vs {ingests})"
    );
}

#[test]
fn low_budget_relay_rejects_before_forward() {
    let pic = PocketIcBuilder::new()
        .with_nns_subnet()
        .with_application_subnet()
        .build();
    let app = pic.topology().get_app_subnets()[0];
    let mock = pic.create_canister_on_subnet(None, None, app);
    pic.add_cycles(mock, T_CYCLES);
    pic.install_canister(mock, mock_wasm(), Encode!().unwrap(), None);

    let relay = pic.create_canister_on_subnet(None, None, app);
    // Below CYCLE_FLOOR (1T) + INGEST_PRICE → the budget floor must reject.
    pic.add_cycles(relay, 900_000_000_000);
    let init = InitArgs {
        index: mock,
        allowlist: vec![pusher()],
        games: vec![],
    };
    pic.install_canister(relay, relay_wasm(), Encode!(&Some(init)).unwrap(), None);

    let res = submit(&pic, relay, pusher(), &Request::Ingest("s".to_string()));
    assert!(matches!(res, SubmitResult::LowBudget), "got {res:?}");
    assert_eq!(q_u64(&pic, mock, "calls"), 0, "no forward on low budget");
}

#[test]
fn mainnet_init_override_traps() {
    let pic = PocketIcBuilder::new().with_application_subnet().build();
    let app = pic.topology().get_app_subnets()[0];
    let relay = pic.create_canister_on_subnet(None, None, app);
    pic.add_cycles(relay, T_CYCLES);

    // A mainnet-profile relay must trap on an init override (spec §Управление).
    let mainnet = build_wasm("target/mainnet-e2e", None, "mainnet");
    let init = InitArgs {
        index: relay,
        allowlist: vec![pusher()],
        games: vec![],
    };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        pic.install_canister(relay, mainnet, Encode!(&Some(init)).unwrap(), None);
    }));
    assert!(
        outcome.is_err(),
        "mainnet install with an override must trap"
    );
}
