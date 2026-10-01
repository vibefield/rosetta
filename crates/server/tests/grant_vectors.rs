//! The shared grant vectors (`tests/fixtures/grants.json`), written by
//! `tools/auth/grant_vectors.py`, a stdlib-Python implementation independent
//! of this crate and of `clients/ts/grants.ts`; both implementations must
//! reproduce every byte, and both list the 401 reasons it pins.

use rosetta_server::Refusal;
use rosetta_server::grant::{self, GrantClaims, GrantError};
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    key_hex: String,
    now_ms: i64,
    valid: Vec<Valid>,
    invalid: Vec<Invalid>,
    refusals: Vec<String>,
}

#[derive(Deserialize)]
struct Valid {
    name: String,
    claims: GrantClaims,
    payload: String,
    grant: String,
}

#[derive(Deserialize)]
struct Invalid {
    name: String,
    grant: String,
    reason: String,
}

fn fixture() -> Fixture {
    serde_json::from_str(include_str!("fixtures/grants.json")).expect("grant vectors parse")
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
        .collect()
}

fn reason(error: GrantError) -> &'static str {
    match error {
        GrantError::Malformed => "malformed",
        GrantError::BadSignature => "bad-signature",
        GrantError::Expired => "expired",
    }
}

#[test]
fn every_valid_vector_is_reproduced_byte_for_byte() {
    let fixture = fixture();
    let key = hex(&fixture.key_hex);
    assert!(!fixture.valid.is_empty());
    for vector in &fixture.valid {
        assert_eq!(
            String::from_utf8(grant::canonical_payload(&vector.claims)).unwrap(),
            vector.payload,
            "{}",
            vector.name
        );
        assert_eq!(
            grant::mint(&key, &vector.claims),
            vector.grant,
            "{}",
            vector.name
        );
        assert_eq!(
            grant::verify(&key, &vector.grant, fixture.now_ms).as_ref(),
            Ok(&vector.claims),
            "{}",
            vector.name
        );
    }
}

#[test]
fn the_refusal_vocabulary_is_the_fixtures() {
    let ours: Vec<&str> = Refusal::ALL
        .iter()
        .map(|refusal| refusal.as_str())
        .collect();
    assert_eq!(ours, fixture().refusals);
}

#[test]
fn every_invalid_vector_is_refused_for_its_reason() {
    let fixture = fixture();
    let key = hex(&fixture.key_hex);
    assert!(!fixture.invalid.is_empty());
    for vector in &fixture.invalid {
        let refused = grant::verify(&key, &vector.grant, fixture.now_ms)
            .map(|claims| format!("accepted {claims:?}"))
            .map_err(reason);
        assert_eq!(refused, Err(vector.reason.as_str()), "{}", vector.name);
    }
}
