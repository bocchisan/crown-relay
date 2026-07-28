//! PocketIC end-to-end for the relay on a real IC replica.
//!
//! Run with the bundled server:
//!   POCKET_IC_BIN=~/.cache/dfinity/versions/<v>/pocket-ic cargo test --test e2e

use candid::{Decode, Encode, Principal};
use crown_relay::config;
use crown_relay::{InitArgs, Request, SignReq, SubmitResult};
use pocket_ic::{PocketIc, PocketIcBuilder};

const T_CYCLES: u128 = 4_000_000_000_000;

/// A fixed allowlisted pusher principal for the tests.
fn pusher() -> Principal {
    Principal::from_slice(&[42u8; 10])
}

fn build_wasm(dir: &str, package_dir: Option<&str>, profile: &str) -> Vec<u8> {
    let out = format!("{dir}/wasm32-unknown-unknown/release/{}.wasm", {
        if package_dir.is_some() {
            "mock_downstream"
        } else {
            "crown_relay"
        }
    });
    let mut cmd = std::process::Command::new("cargo");
    cmd.args([
        "build",
        "--lib",
        "--release",
        "--target",
        "wasm32-unknown-unknown",
        "--target-dir",
        dir,
    ]);
    cmd.env("CROWN_PROFILE", profile);
    if let Some(p) = package_dir {
        cmd.current_dir(p);
    }
    assert!(cmd.status().expect("cargo build").success(), "build failed");
    std::fs::read(&out).unwrap_or_else(|_| panic!("read {out}"))
}

fn relay_wasm() -> Vec<u8> {
    build_wasm("target", None, "testnet")
}

fn mock_wasm() -> Vec<u8> {
    build_wasm("e2e-mock/target", Some("e2e-mock"), "testnet")
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
        Encode!(&Request::Settlement("sig".to_string())).unwrap(),
    );
    assert!(res.is_err(), "a stranger must be rejected, not executed");
    assert_eq!(q_u64(&pic, mock, "calls"), 0, "no forward happened");
}

#[test]
fn allowlisted_settlement_forwards_to_index_with_ingest_price() {
    let (pic, relay, mock) = setup();
    let res = submit(
        &pic,
        relay,
        pusher(),
        &Request::Settlement("sig-1".to_string()),
    );
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
    let req = Request::Sign(SignReq {
        game: mock, // the mock also serves the game role
        request: Encode!().unwrap(),
    });
    let res = submit(&pic, relay, pusher(), &req);
    assert!(matches!(res, SubmitResult::Forwarded(_)), "got {res:?}");
    assert_eq!(q_string(&pic, mock, "last_method"), "request_signature");
    assert_eq!(q_u64(&pic, mock, "last_cycles"), config::SIGN_PRICE as u64);
}

#[test]
fn sign_to_an_unknown_game_is_rejected_without_forwarding() {
    let (pic, relay, mock) = setup(); // games = [mock]
    let rogue = Principal::from_slice(&[9u8; 10]); // not an allowlisted game
    let req = Request::Sign(SignReq {
        game: rogue,
        request: Encode!().unwrap(),
    });
    let res = submit(&pic, relay, pusher(), &req);
    assert!(matches!(res, SubmitResult::UnknownGame), "got {res:?}");
    // No cycles left the budget — the drain is blocked before the forward.
    assert_eq!(q_u64(&pic, mock, "calls"), 0, "no forward to an unknown game");
}

#[test]
fn per_key_rate_limit_caps_a_key() {
    let (pic, relay, mock) = setup();
    let limit = config::RATE_LIMIT;
    for _ in 0..limit {
        let res = submit(&pic, relay, pusher(), &Request::Settlement("s".to_string()));
        assert!(matches!(res, SubmitResult::Forwarded(_)));
    }
    // The next submit in the window is capped — no further forward.
    let capped = submit(&pic, relay, pusher(), &Request::Settlement("s".to_string()));
    assert!(
        matches!(capped, SubmitResult::RateLimited),
        "got {capped:?}"
    );
    assert_eq!(q_u64(&pic, mock, "calls"), u64::from(limit));
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

    let res = submit(&pic, relay, pusher(), &Request::Settlement("s".to_string()));
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
