"""The shared grant vectors (docs/AUTH.md section 5), written by an implementation
independent of both crates/server/src/grant.rs and clients/ts/grants.ts.

    python3 tools/auth/grant_vectors.py > crates/server/tests/fixtures/grants.json

Stdlib only (json, hmac, hashlib, base64); the output is deterministic, so a rerun
reproduces the committed fixture byte for byte.

Canonical payload: RFC 8785 for this shape == json.dumps(sort_keys=True,
separators=(",", ":"), ensure_ascii=False) because every member name is ASCII
(code-point order == UTF-16 code-unit order), every number is a safe integer,
and Python escapes exactly what ECMAScript's JSON.stringify escapes for
well-formed strings (", \\, the C0 controls; \\b \\f \\n \\r \\t short; others
\\u00xx lowercase; DEL, U+2028 and non-ASCII raw).
"""
import base64, hashlib, hmac, json, sys

KEY = bytes(range(32))
OTHER_KEY = bytes(range(32, 64))
NOW = 1_790_000_000_000


def b64(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode("ascii")


def canonical(claims) -> bytes:
    return json.dumps(claims, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")


def sign(key: bytes, payload: bytes) -> str:
    return b64(payload) + "." + b64(hmac.new(key, payload, hashlib.sha256).digest())


def claims(**overrides):
    base = {"v": 1, "sid": "s1", "cid": "fieldd/window-1", "exp": NOW + 60_000, "scope": ["read"]}
    base.update(overrides)
    return base


valid = []
for name, c in [
    ("a read grant", claims()),
    (
        "escapes, controls and non-ASCII",
        claims(
            sid='s"1\\2\u0001\n\t\u007f/é \U0001F600',
            cid="phone:über/\u0000x",
            exp=NOW + 1,
        ),
    ),
    ("an unknown scope entry is carried", claims(scope=["read", "x-later"], exp=NOW + 3_600_000)),
    # RO-13: the recorded-file read's scope rides beside `read`, in the minter's order (JCS sorts
    # members, never array entries).
    ("a read and files grant", claims(scope=["read", "files"])),
    ("the largest safe expiry", claims(exp=9_007_199_254_740_991)),
]:
    payload = canonical(c)
    valid.append({"name": name, "claims": c, "payload": payload.decode("utf-8"), "grant": sign(KEY, payload)})

good = canonical(claims())
good_grant = sign(KEY, good)
good_payload_b64, good_mac_b64 = good_grant.split(".")
tampered = canonical(claims(sid="s2"))

# The same 32 MAC bytes spelled non-canonically: a 43-character encoding
# carries 2 trailing bits that a strict decoder requires to be zero. A lenient
# decoder (Python's own, for one) reads this as the authentic MAC.
ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
last = ALPHABET.index(good_mac_b64[-1])
assert last & 0b11 == 0, "a canonical 43-character encoding ends in zero trailing bits"
trailing_bits_mac = good_mac_b64[:-1] + ALPHABET[last | 0b01]
assert base64.urlsafe_b64decode(trailing_bits_mac + "=") == base64.urlsafe_b64decode(good_mac_b64 + "=")

invalid = [
    {"name": "expired a millisecond ago", "grant": sign(KEY, canonical(claims(exp=NOW - 1))), "reason": "expired"},
    {"name": "expiry equal to now", "grant": sign(KEY, canonical(claims(exp=NOW))), "reason": "expired"},
    {"name": "minted under another key", "grant": sign(OTHER_KEY, good), "reason": "bad-signature"},
    {"name": "claims changed under the original MAC", "grant": b64(tampered) + "." + good_mac_b64, "reason": "bad-signature"},
    {"name": "whitespace in the payload", "grant": sign(KEY, json.dumps(claims(), sort_keys=True).encode()), "reason": "malformed"},
    {
        "name": "members out of order",
        "grant": sign(KEY, json.dumps(claims(), separators=(",", ":")).encode()),
        "reason": "malformed",
    },
    {"name": "an unknown member", "grant": sign(KEY, canonical(dict(claims(), iat=NOW))), "reason": "malformed"},
    {"name": "version 2", "grant": sign(KEY, canonical(claims(v=2))), "reason": "malformed"},
    {"name": "an empty session id", "grant": sign(KEY, canonical(claims(sid=""))), "reason": "malformed"},
    {
        "name": "an expiry past the safe-integer range",
        "grant": sign(KEY, canonical(claims(exp=9_007_199_254_740_992))),
        "reason": "malformed",
    },
    {
        "name": "a fractional expiry",
        "grant": sign(KEY, good.replace(str(NOW + 60_000).encode(), (str(NOW + 60_000) + ".5").encode())),
        "reason": "malformed",
    },
    {"name": "a padded payload", "grant": base64.urlsafe_b64encode(good).decode() + "." + good_mac_b64, "reason": "malformed"},
    {"name": "no MAC", "grant": good_payload_b64, "reason": "malformed"},
    {"name": "a short MAC", "grant": good_payload_b64 + "." + good_mac_b64[:-4], "reason": "malformed"},
    {"name": "standard-alphabet base64", "grant": "+" + good_grant, "reason": "malformed"},
    {
        "name": "the authentic MAC spelled with non-zero trailing bits",
        "grant": good_payload_b64 + "." + trailing_bits_mac,
        "reason": "malformed",
    },
]

# The 401 reasons (docs/AUTH.md section 6), in the order both implementations list them.
REFUSALS = [
    "missing", "bad-token", "malformed", "bad-signature", "expired",
    "wrong-session", "wrong-scope", "wrong-route", "grants-disabled",
]

fixture = {
    "rule": (
        "payload = RFC 8785 canonical JSON of {cid, exp, scope, sid, v}; "
        "grant = base64url(payload) '.' base64url(HMAC-SHA256(key, payload)), no padding; "
        "verification: MAC first, then the payload must equal its own canonical form, v == 1, "
        "non-empty sid and cid, 0 <= exp <= 2^53-1, exp > now (docs/AUTH.md)"
    ),
    "generator": "tools/auth/grant_vectors.py (stdlib Python: json, hmac, hashlib, base64), independent of both implementations",
    "key_hex": KEY.hex(),
    "now_ms": NOW,
    "valid": valid,
    "invalid": invalid,
    "refusals": REFUSALS,
}
json.dump(fixture, sys.stdout, indent=2, ensure_ascii=True)
sys.stdout.write("\n")
