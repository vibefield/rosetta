//! Per-session expiring grants (RO-10): a capability the daemon's
//! supervisor mints so one client can read ONE session's recorded history
//! without holding the daemon bearer. docs/AUTH.md is the rule; the
//! TypeScript twin is `clients/ts/grants.ts`, and both sides read the same
//! vectors (`crates/server/tests/fixtures/grants.json`).
//!
//! A grant is `<b64url(payload)>.<b64url(mac)>`: `payload` is the canonical
//! JSON of [`GrantClaims`], `mac` is HMAC-SHA256 over exactly those payload
//! bytes. The MAC is checked BEFORE the payload is parsed, and the payload
//! must be byte-identical to its own canonical re-encoding, so there is one
//! accepted spelling of every grant.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

/// The only payload version this daemon accepts.
pub const GRANT_VERSION: u32 = 1;
/// A shorter key is a supervisor bug, refused at startup.
pub const MIN_GRANT_KEY_BYTES: usize = 32;
/// The largest encoded grant (payload and MAC) a request may present.
pub const MAX_GRANT_BYTES: usize = 4096;
/// The scope every grant-openable route needs.
pub const SCOPE_READ: &str = "read";
/// The recorded-file read (`/read/{target_index}`), which reads a file from
/// the daemon's disk by the transcript's reference — more than the
/// transcript, so it is minted separately. The file route needs it beside
/// [`SCOPE_READ`]; no other route reads it (docs/AUTH.md §5.4).
pub const SCOPE_FILES: &str = "files";
/// JavaScript's `Number.MAX_SAFE_INTEGER`: an expiry past it has no exact
/// canonical spelling on the TypeScript side, so it is malformed here.
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;
const MAC_BYTES: usize = 32;

/// The claims of a version-1 grant. The field order IS the canonical member
/// order (RFC 8785 sorts members by UTF-16 code unit; every name is ASCII).
/// Unknown members are not rejected by serde here: the canonical re-encoding
/// drops them, the bytes then differ, and the grant is malformed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantClaims {
    /// The minter's name for the client. Logged; nothing is bound to it.
    pub cid: String,
    /// Expiry in Unix milliseconds; the grant is refused once `exp <= now`.
    pub exp: i64,
    /// What the grant may do. Routes need [`SCOPE_READ`]; the recorded-file
    /// route needs [`SCOPE_FILES`] as well. Unknown entries are carried and
    /// ignored.
    pub scope: Vec<String>,
    /// The canonical session id this grant opens.
    pub sid: String,
    /// Payload version, always [`GRANT_VERSION`].
    pub v: u32,
}

impl GrantClaims {
    /// A version-1 read grant for one session.
    pub fn read(sid: impl Into<String>, cid: impl Into<String>, exp: i64) -> Self {
        Self {
            cid: cid.into(),
            exp,
            scope: vec![SCOPE_READ.to_string()],
            sid: sid.into(),
            v: GRANT_VERSION,
        }
    }

    pub fn allows(&self, scope: &str) -> bool {
        self.scope.iter().any(|entry| entry == scope)
    }
}

/// Why a presented grant failed on its own terms (before any route check).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantError {
    /// Not `b64url.b64url`, bad base64, a MAC of the wrong length, a payload
    /// that is not the canonical encoding of version-1 claims.
    Malformed,
    /// The MAC does not verify under this daemon's key.
    BadSignature,
    /// Authentic, but `exp` is not in the future.
    Expired,
}

/// The canonical payload bytes of `claims` (RFC 8785 for this shape: members
/// in code-unit order, no whitespace, strings escaped as ECMAScript's
/// `JSON.stringify` does, integers in plain decimal).
pub fn canonical_payload(claims: &GrantClaims) -> Vec<u8> {
    serde_json::to_vec(claims).expect("grant claims always serialize")
}

fn mac(key: &[u8], payload: &[u8]) -> Hmac<Sha256> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(payload);
    mac
}

/// Mint a grant. The supervisor (fieldd) is the production minter; this is
/// the reference the vectors and the daemon's tests use.
pub fn mint(key: &[u8], claims: &GrantClaims) -> String {
    let payload = canonical_payload(claims);
    let tag = mac(key, &payload).finalize().into_bytes();
    format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(&payload),
        URL_SAFE_NO_PAD.encode(tag)
    )
}

/// Verify a grant's MAC, form, version and expiry. Route checks (the session
/// and the scope) are the caller's: this answers only "is it a live grant".
pub fn verify(key: &[u8], grant: &str, now_ms: i64) -> Result<GrantClaims, GrantError> {
    let claims = open(key, grant)?;
    if claims.exp <= now_ms {
        return Err(GrantError::Expired);
    }
    Ok(claims)
}

/// Authenticate a grant (MAC, canonical form, version) WITHOUT judging its
/// expiry, so a caller can name the client of an expired grant: once the MAC
/// verifies, the claims are the minter's own words.
pub fn open(key: &[u8], grant: &str) -> Result<GrantClaims, GrantError> {
    if grant.len() > MAX_GRANT_BYTES {
        return Err(GrantError::Malformed);
    }
    let (payload, tag) = grant.split_once('.').ok_or(GrantError::Malformed)?;
    // No padding and no trailing bits: each byte string has one spelling.
    let payload = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| GrantError::Malformed)?;
    let tag = URL_SAFE_NO_PAD
        .decode(tag)
        .map_err(|_| GrantError::Malformed)?;
    if tag.len() != MAC_BYTES {
        return Err(GrantError::Malformed);
    }
    // Constant-time comparison; nothing attacker-shaped is parsed before it.
    mac(key, &payload)
        .verify_slice(&tag)
        .map_err(|_| GrantError::BadSignature)?;
    let claims: GrantClaims =
        serde_json::from_slice(&payload).map_err(|_| GrantError::Malformed)?;
    if canonical_payload(&claims) != payload
        || claims.v != GRANT_VERSION
        || claims.sid.is_empty()
        || claims.cid.is_empty()
        || !(0..=MAX_SAFE_INTEGER).contains(&claims.exp)
    {
        return Err(GrantError::Malformed);
    }
    Ok(claims)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";
    const NOW: i64 = 1_790_000_000_000;

    fn claims() -> GrantClaims {
        GrantClaims::read("s1", "fieldd/window-1", NOW + 60_000)
    }

    /// Re-encode `payload` under `key` without canonicalizing it — what a
    /// minter with the right key but the wrong encoder would produce.
    fn sign_raw(key: &[u8], payload: &[u8]) -> String {
        let tag = mac(key, payload).finalize().into_bytes();
        format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(payload),
            URL_SAFE_NO_PAD.encode(tag)
        )
    }

    #[test]
    fn a_minted_grant_verifies_to_its_claims() {
        let grant = mint(KEY, &claims());
        assert_eq!(verify(KEY, &grant, NOW), Ok(claims()));
        assert_eq!(
            String::from_utf8(canonical_payload(&claims())).unwrap(),
            format!(
                r#"{{"cid":"fieldd/window-1","exp":{},"scope":["read"],"sid":"s1","v":1}}"#,
                NOW + 60_000
            )
        );
    }

    #[test]
    fn expiry_is_strict() {
        let grant = mint(KEY, &claims());
        assert_eq!(verify(KEY, &grant, NOW + 59_999), Ok(claims()));
        assert_eq!(verify(KEY, &grant, NOW + 60_000), Err(GrantError::Expired));
    }

    #[test]
    fn another_key_or_a_flipped_bit_is_a_bad_signature() {
        let grant = mint(KEY, &claims());
        assert_eq!(
            verify(b"another key, also thirty-two by", &grant, NOW),
            Err(GrantError::BadSignature)
        );
        let (payload, _) = grant.split_once('.').unwrap();
        let forged = mint(b"forger's key of 32 bytes, at le", &claims());
        let (_, forged_tag) = forged.split_once('.').unwrap();
        assert_eq!(
            verify(KEY, &format!("{payload}.{forged_tag}"), NOW),
            Err(GrantError::BadSignature)
        );
        // A changed claim under the original MAC.
        let mut widened = claims();
        widened.sid = "s2".into();
        let widened_payload = URL_SAFE_NO_PAD.encode(canonical_payload(&widened));
        let (_, tag) = grant.split_once('.').unwrap();
        assert_eq!(
            verify(KEY, &format!("{widened_payload}.{tag}"), NOW),
            Err(GrantError::BadSignature)
        );
    }

    #[test]
    fn only_the_canonical_spelling_is_accepted_even_when_signed() {
        let canonical = canonical_payload(&claims());
        let text = String::from_utf8(canonical.clone()).unwrap();
        for payload in [
            // whitespace
            text.replace(",", ", "),
            // member order
            format!(
                r#"{{"sid":"s1","cid":"fieldd/window-1","exp":{},"scope":["read"],"v":1}}"#,
                NOW + 60_000
            ),
            // an unknown member
            text.replace(r#""v":1"#, r#""v":1,"iat":1"#),
            // a number spelled another way
            text.replace(&format!("{}", NOW + 60_000), &format!("{}.0", NOW + 60_000)),
            // an escape JSON.stringify would not write
            text.replace("fieldd/window-1", r"fieldd\/window-1"),
        ] {
            assert_eq!(
                verify(KEY, &sign_raw(KEY, payload.as_bytes()), NOW),
                Err(GrantError::Malformed),
                "{payload}"
            );
        }
        assert_eq!(verify(KEY, &sign_raw(KEY, &canonical), NOW), Ok(claims()));
    }

    #[test]
    fn version_ids_and_expiry_range_are_checked_after_the_mac() {
        let mut v2 = claims();
        v2.v = 2;
        let mut no_sid = claims();
        no_sid.sid.clear();
        let mut no_cid = claims();
        no_cid.cid.clear();
        let mut unsafe_exp = claims();
        unsafe_exp.exp = MAX_SAFE_INTEGER + 1;
        for bad in [v2, no_sid, no_cid, unsafe_exp] {
            assert_eq!(
                verify(KEY, &mint(KEY, &bad), NOW),
                Err(GrantError::Malformed),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn encoding_errors_are_malformed() {
        let grant = mint(KEY, &claims());
        let (payload, tag) = grant.split_once('.').unwrap();
        for bad in [
            String::new(),
            payload.to_string(),
            format!("{payload}.{tag}.x"),
            // padding, and the standard alphabet's `+`
            format!("{payload}=.{tag}"),
            format!("{payload}.{tag}="),
            format!("+{payload}.{tag}"),
            // a MAC of the wrong length
            format!("{payload}.{}", &tag[..tag.len() - 2]),
            format!("{payload}.{}", URL_SAFE_NO_PAD.encode([0u8; 16])),
            format!("{payload}.{tag}{}", "A".repeat(MAX_GRANT_BYTES)),
        ] {
            assert_eq!(verify(KEY, &bad, NOW), Err(GrantError::Malformed), "{bad}");
        }
        // Authentic bytes that are not JSON at all.
        assert_eq!(
            verify(KEY, &sign_raw(KEY, b"not json"), NOW),
            Err(GrantError::Malformed)
        );
    }
}
