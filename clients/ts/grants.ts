// Per-session expiring grants (docs/AUTH.md): the TypeScript twin of
// `crates/server/src/grant.rs`, for the supervisor that mints them (fieldd)
// and for its tests. Node-only — it signs with `node:crypto` — so it is its
// own package entry, `@rosetta/protocol/grants`, never part of the browser
// client. A browser carries a grant as an opaque string (the client's
// `credential` option); only the key's holder mints or verifies one.
//
// A grant is `<base64url(payload)>.<base64url(HMAC-SHA256(key, payload))>`,
// unpadded. `payload` is the RFC 8785 canonical JSON of the claims below.
// Both sides read `crates/server/tests/fixtures/grants.json`.

import { createHmac, timingSafeEqual } from "node:crypto";
import { base64UrlDecode, base64UrlEncode } from "#base64url";

/** The only payload version a daemon accepts. */
export const GRANT_VERSION = 1;
/** The scope every grant-openable route needs. */
export const SCOPE_READ = "read";
/** A daemon refuses to start with a shorter key. */
export const MIN_GRANT_KEY_BYTES = 32;
/** The largest encoded grant a daemon reads. */
export const MAX_GRANT_BYTES = 4096;
const MAC_BYTES = 32;

export interface GrantClaims {
  /** The minter's name for the client. The daemon logs it; nothing is bound to it. */
  readonly cid: string;
  /** Expiry in Unix milliseconds, a safe integer; refused once `exp <= now`. */
  readonly exp: number;
  /** What the grant may do; routes need {@link SCOPE_READ}. Unknown entries are carried. */
  readonly scope: readonly string[];
  /** The canonical session id the grant opens. */
  readonly sid: string;
  readonly v: 1;
}

/** Why a grant fails on its own terms (route checks are the daemon's). */
export type GrantRefusal = "malformed" | "bad-signature" | "expired";

export type GrantVerdict =
  | { readonly ok: true; readonly claims: GrantClaims }
  | { readonly ok: false; readonly reason: GrantRefusal };

export type JsonValue =
  | null
  | boolean
  | number
  | string
  | readonly JsonValue[]
  | { readonly [key: string]: JsonValue };

/** A lone UTF-16 surrogate: not Unicode text, so not I-JSON (RFC 8785 §3.1). */
const LONE_SURROGATE = /\p{Cs}/u;

function isArray(value: JsonValue): value is readonly JsonValue[] {
  return Array.isArray(value);
}

/**
 * RFC 8785 (JCS): members sorted by UTF-16 code units (JavaScript's default
 * sort), no whitespace, strings and numbers exactly as `JSON.stringify`
 * writes them. Throws on what I-JSON forbids: non-finite numbers and lone
 * surrogates.
 */
export function canonicalize(value: JsonValue): string {
  if (typeof value === "string") {
    if (LONE_SURROGATE.test(value)) throw new TypeError("canonical JSON strings are Unicode text");
    return JSON.stringify(value);
  }
  if (typeof value === "number") {
    if (!Number.isFinite(value)) throw new TypeError("canonical JSON numbers are finite");
    return JSON.stringify(value);
  }
  if (value === null || typeof value === "boolean") return JSON.stringify(value);
  if (isArray(value)) return `[${value.map(canonicalize).join(",")}]`;
  const members = Object.keys(value)
    .sort()
    .flatMap((key) => {
      const member = value[key];
      return member === undefined ? [] : [`${canonicalize(key)}:${canonicalize(member)}`];
    });
  return `{${members.join(",")}}`;
}

/** The canonical payload text of `claims` (exactly the five members). */
export function canonicalPayload(claims: GrantClaims): string {
  return canonicalize({
    cid: claims.cid,
    exp: claims.exp,
    scope: [...claims.scope],
    sid: claims.sid,
    v: claims.v,
  });
}

function isClaims(value: unknown): value is GrantClaims {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return false;
  const claims = value as Record<string, unknown>;
  return (
    claims.v === GRANT_VERSION &&
    typeof claims.sid === "string" &&
    claims.sid !== "" &&
    typeof claims.cid === "string" &&
    claims.cid !== "" &&
    typeof claims.exp === "number" &&
    Number.isSafeInteger(claims.exp) &&
    claims.exp >= 0 &&
    Array.isArray(claims.scope) &&
    claims.scope.every((entry) => typeof entry === "string")
  );
}

/** Canonical version-1 claims from payload bytes, or `null`. */
function claimsFrom(payload: Uint8Array): GrantClaims | null {
  try {
    const text = new TextDecoder("utf-8", { fatal: true }).decode(payload);
    const value: unknown = JSON.parse(text);
    // One spelling per grant: the payload must be its own canonical form.
    return isClaims(value) && canonicalPayload(value) === text ? value : null;
  } catch {
    return null;
  }
}

function sign(key: Uint8Array, payload: Uint8Array): Uint8Array {
  return createHmac("sha256", key).update(payload).digest();
}

/** Mint a grant. Throws on claims a daemon would call malformed, or a short key. */
export function mintGrant(key: Uint8Array, claims: GrantClaims): string {
  if (key.length < MIN_GRANT_KEY_BYTES) {
    throw new RangeError(`a grant key is at least ${MIN_GRANT_KEY_BYTES} bytes`);
  }
  if (!isClaims(claims)) throw new TypeError("not version-1 grant claims");
  const payload = new TextEncoder().encode(canonicalPayload(claims));
  return `${base64UrlEncode(payload)}.${base64UrlEncode(sign(key, payload))}`;
}

/**
 * Verify as the daemon does: the MAC first (constant-time), then the
 * canonical form, version, ids and expiry range, then `exp > nowMs`.
 */
export function verifyGrant(key: Uint8Array, grant: string, nowMs: number): GrantVerdict {
  const refused = (reason: GrantRefusal): GrantVerdict => ({ ok: false, reason });
  if (grant.length > MAX_GRANT_BYTES) return refused("malformed");
  const dot = grant.indexOf(".");
  if (dot < 0) return refused("malformed");
  const payload = base64UrlDecode(grant.slice(0, dot));
  const mac = base64UrlDecode(grant.slice(dot + 1));
  if (payload === null || mac === null || mac.length !== MAC_BYTES) return refused("malformed");
  if (!timingSafeEqual(sign(key, payload), mac)) return refused("bad-signature");
  const claims = claimsFrom(payload);
  if (claims === null) return refused("malformed");
  if (claims.exp <= nowMs) return refused("expired");
  return { ok: true, claims };
}

/** The `Authorization` value that presents `grant`. */
export function grantAuthorization(grant: string): string {
  return `Rosetta-Grant ${grant}`;
}

/**
 * Read a grant — an `Authorization: Rosetta-Grant <grant>` value or the bare
 * grant — WITHOUT verifying it, e.g. to learn when a held grant expires.
 * `null` unless it carries canonical version-1 claims. Only a key holder
 * can tell whether it is authentic.
 */
export function parseGrantHeader(value: string): { grant: string; claims: GrantClaims } | null {
  const text = value.trim();
  const scheme = /^rosetta-grant\s+/i.exec(text);
  const grant = scheme === null ? text : text.slice(scheme[0].length);
  const dot = grant.indexOf(".");
  if (dot < 0 || grant.length > MAX_GRANT_BYTES) return null;
  const payload = base64UrlDecode(grant.slice(0, dot));
  const mac = base64UrlDecode(grant.slice(dot + 1));
  if (payload === null || mac === null || mac.length !== MAC_BYTES) return null;
  const claims = claimsFrom(payload);
  return claims === null ? null : { grant, claims };
}
