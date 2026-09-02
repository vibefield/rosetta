// A site's own icon, fetched by the process that is allowed to touch the web.
//
// The transcript's web lines carry a url and a title and nothing else, so the
// icon has to be fetched — and the design record is explicit about where from:
// `https://<host>/favicon.ico`, the site itself. NEVER a favicon service.
// Asking Google or DuckDuckGo for the icon of every site an agent ever
// searched hands them the user's whole agentic past one host at a time, and
// §9's voice is that where your words go is your business.
//
// It runs HERE rather than in the renderer for the same reason
// `ah-file-open-external` does: the renderer ASKS, the shell decides. A
// renderer drawing content from transcripts nobody here wrote is the process
// whose word on "make this network request" is worth least. So every host is
// re-derived and re-validated on this side, on the first request AND on every
// redirect hop, and nothing but a validated public https host is ever dialled.
//
// Two rules make the result trustworthy rather than merely fetched:
//
//   * The bytes decide what the bytes are. A `Content-Type` header is a claim
//     by a server we do not trust; the leading bytes are evidence. Only ICO,
//     PNG, GIF, JPEG, WebP and SVG are admitted, and an SVG is admitted only
//     because the renderer draws it through `<img>`, where scripting is
//     inert by specification.
//   * A refusal is cached too. A site with no icon must not be re-dialled on
//     every scroll — that would be the surveillance this design avoided by
//     not using a favicon service, merely inflicted on the site instead.
//
// Nothing here is Electron-specific: `createFaviconService` takes its cache
// directory, its fetch and its clock, so the test drives it with an injected
// fetch and never touches the network. `main.mjs` supplies the real three.

import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { createHash, randomUUID } from "node:crypto";
import path from "node:path";

/** Long enough for a slow site, short enough that no line waits on one. */
export const FETCH_TIMEOUT_MS = 5_000;
/** A favicon is small. Anything larger is not an icon, whatever it claims. */
export const MAX_ICON_BYTES = 256 * 1024;
/** An icon that answered. Sites change them rarely; a month is honest. */
export const POSITIVE_TTL_MS = 30 * 24 * 60 * 60 * 1000;
/** A refusal, a 404, a timeout. Short, because the answer may change. */
export const NEGATIVE_TTL_MS = 24 * 60 * 60 * 1000;
/** Redirects are followed by hand so each hop is re-validated. Three is more
 * than any real favicon needs and fewer than a loop requires. */
export const MAX_REDIRECTS = 3;

/** Names that resolve inside the machine or the LAN, or resolve nowhere:
 * RFC 6761 special-use, RFC 6762 mDNS, RFC 7686 onion, and the reverse-DNS
 * tree. A favicon never lives at any of them, and dialling one would let a
 * transcript point this process at the user's own network. */
const RESERVED_SUFFIXES = [
  "localhost",
  "local",
  "internal",
  "intranet",
  "private",
  "corp",
  "home",
  "lan",
  "home.arpa",
  "arpa",
  "test",
  "example",
  "invalid",
  "onion",
];

/** Characters that mean the caller handed over more than a hostname — a path,
 * a query, a fragment, credentials, a port, a scheme, or whitespace. Rejected
 * BEFORE parsing, because `new URL` would helpfully strip them and hand back
 * a hostname that looks clean. */
const NOT_IN_A_HOSTNAME = /[^\p{L}\p{N}.\-]/u;

/** One DNS label: alphanumeric at both ends, hyphens allowed inside. Applied
 * after `URL` has done the IDNA/punycode conversion, so a unicode host is
 * checked in the ASCII form that would actually be dialled. */
const LABEL = /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/;

/**
 * The hostname this service is willing to dial, or `null`.
 *
 * Returns the ASCII (punycode) form, which is what a request would use and
 * therefore the only form worth caching under. Refuses everything that is not
 * a bare public domain name: paths, credentials, ports, schemes, IP literals
 * of either family, single labels, reserved suffixes, and anything over the
 * DNS length limits.
 *
 * IP literals are refused wholesale rather than filtered to private ranges.
 * A search result's source is always a domain name, so the stricter rule
 * costs nothing — and a private-range table is a list of the edge cases
 * somebody remembered (`0.0.0.0/8`, `169.254/16`, `100.64/10`, IPv4-mapped
 * IPv6, decimal and octal literals), which is a worse thing to be right by.
 */
export function normalizeHost(raw) {
  if (typeof raw !== "string") return null;
  // One trailing dot is legal FQDN syntax and names the same host.
  const trimmed = raw.trim().replace(/\.$/, "");
  if (trimmed === "" || trimmed.length > 253) return null;
  if (NOT_IN_A_HOSTNAME.test(trimmed)) return null;
  // An IPv6 literal arrives bracketed, and the brackets are already refused
  // above; this only re-states the intent for a reader.
  if (trimmed.startsWith("[")) return null;

  let url;
  try {
    url = new URL(`https://${trimmed}/`);
  } catch {
    return null;
  }
  // `URL` is permissive by design; this is the audit of what it produced.
  if (
    url.protocol !== "https:" ||
    url.username !== "" ||
    url.password !== "" ||
    url.port !== "" ||
    url.pathname !== "/" ||
    url.search !== "" ||
    url.hash !== ""
  ) {
    return null;
  }

  const host = url.hostname;
  if (host.length > 253 || host.startsWith("[")) return null;
  const labels = host.split(".");
  // A single label is either a machine on the LAN or a search-domain
  // completion. Neither is a site.
  if (labels.length < 2) return null;
  if (!labels.every((label) => LABEL.test(label))) return null;

  // An all-numeric last label is an IPv4 literal wearing a domain's shape.
  const tld = labels[labels.length - 1];
  if (tld.length < 2 || /^[0-9]+$/.test(tld)) return null;

  for (const suffix of RESERVED_SUFFIXES) {
    if (host === suffix || host.endsWith(`.${suffix}`)) return null;
  }
  return host;
}

/** `url` → the host this service would fetch an icon for, or `null`. */
export function hostOfUrl(raw) {
  if (typeof raw !== "string") return null;
  try {
    const url = new URL(raw);
    if (url.protocol !== "https:" && url.protocol !== "http:") return null;
    return normalizeHost(url.hostname);
  } catch {
    return null;
  }
}

function startsWith(bytes, signature) {
  if (bytes.length < signature.length) return false;
  return signature.every((byte, index) => bytes[index] === byte);
}

/**
 * The media type these bytes ARE, or `null`.
 *
 * The response's own `Content-Type` is deliberately not consulted. It is a
 * string a server we do not trust chose to send, and trusting it is how a
 * `text/html` error page becomes an "icon". The leading bytes are the only
 * evidence in the exchange.
 */
export function sniffImage(bytes) {
  if (!(bytes instanceof Uint8Array) || bytes.length < 4) return null;
  // ICO is `00 00 01 00`. `00 00 02 00` is a CUR — a cursor, not an icon.
  if (startsWith(bytes, [0x00, 0x00, 0x01, 0x00])) return "image/x-icon";
  if (startsWith(bytes, [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])) return "image/png";
  if (startsWith(bytes, [0x47, 0x49, 0x46, 0x38])) return "image/gif";
  if (startsWith(bytes, [0xff, 0xd8, 0xff])) return "image/jpeg";
  // `RIFF....WEBP` — the four size bytes sit between the two tags.
  if (
    bytes.length >= 12 &&
    startsWith(bytes, [0x52, 0x49, 0x46, 0x46]) &&
    bytes[8] === 0x57 &&
    bytes[9] === 0x45 &&
    bytes[10] === 0x42 &&
    bytes[11] === 0x50
  ) {
    return "image/webp";
  }
  // SVG has no magic number, so the evidence is structural: markup whose
  // first element is an `<svg`. An SVG reaches the reader through `<img>`,
  // where script and external references are inert by specification — which
  // is the whole reason it can be admitted at all.
  const head = new TextDecoder("utf-8", { fatal: false })
    .decode(bytes.subarray(0, 1024))
    .replace(/^\uFEFF/, "")
    .trimStart();
  if (head.startsWith("<?xml") || head.startsWith("<!--") || head.startsWith("<svg")) {
    if (/<svg[\s>]/i.test(head)) return "image/svg+xml";
  }
  return null;
}

/** Read a body with a hard ceiling, so a site cannot answer a 14px icon with
 * a gigabyte. The stream is cancelled the moment the cap is passed. */
async function readCapped(body, cap) {
  if (body === null || body === undefined) return null;
  const reader = body.getReader();
  const chunks = [];
  let total = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      total += value.byteLength;
      if (total > cap) {
        await reader.cancel();
        return null;
      }
      chunks.push(value);
    }
  } catch {
    return null;
  } finally {
    reader.releaseLock?.();
  }
  const out = new Uint8Array(total);
  let at = 0;
  for (const chunk of chunks) {
    out.set(chunk, at);
    at += chunk.byteLength;
  }
  return out;
}

function toDataUrl(mime, bytes) {
  return `data:${mime};base64,${Buffer.from(bytes).toString("base64")}`;
}

/** The cache file for a host. Named by digest rather than by the host itself:
 * validation already guarantees a filesystem-safe name, and this guarantees
 * it a second time, from a different direction, for free. The host is stored
 * inside the record so the folder stays readable to a person. */
function cacheFileFor(dir, host) {
  return path.join(dir, `${createHash("sha256").update(host).digest("hex")}.json`);
}

/**
 * The service.
 *
 * `get(host)` resolves to a `data:` URL or `null` — null being every honest
 * "no": a host this process will not dial, a site with no icon, a 404, a
 * timeout, a body that was not an image. The caller draws the globe for all
 * of them, because to a reader they are the same fact.
 *
 * @param {object} options
 * @param {string} options.cacheDir  Where records live; created on demand.
 * @param {typeof fetch} [options.fetch]  Injected for tests; defaults to global.
 * @param {() => number} [options.now]  Injected for TTL tests.
 */
export function createFaviconService({
  cacheDir,
  fetch: fetchImpl = globalThis.fetch,
  now = Date.now,
} = {}) {
  if (typeof cacheDir !== "string" || cacheDir === "") {
    throw new Error("favicon service needs a cache directory");
  }

  /** Settled records, by host. The disk is the durable copy; this is the one
   * that answers a scrolling ledger without a syscall per line. */
  const memory = new Map();
  /** In-flight work, by host. A ledger drawing eight lines from one site asks
   * eight times in the same tick and must dial once. */
  const inFlight = new Map();
  let dirReady = null;

  function fresh(record) {
    if (record === null || typeof record !== "object") return false;
    if (typeof record.fetchedAt !== "number") return false;
    const ttl = typeof record.dataUrl === "string" ? POSITIVE_TTL_MS : NEGATIVE_TTL_MS;
    const age = now() - record.fetchedAt;
    // A record from the future is a clock that moved, not a fresh icon.
    return age >= 0 && age < ttl;
  }

  async function readRecord(host) {
    try {
      const raw = await readFile(cacheFileFor(cacheDir, host), "utf8");
      const record = JSON.parse(raw);
      // A record filed under a digest still states its host; a mismatch means
      // the file is not about this host, whatever its name says.
      if (record?.host !== host) return null;
      if (record.dataUrl !== null && typeof record.dataUrl !== "string") return null;
      return record;
    } catch {
      return null;
    }
  }

  async function writeRecord(record) {
    try {
      dirReady ??= mkdir(cacheDir, { recursive: true });
      await dirReady;
      const target = cacheFileFor(cacheDir, record.host);
      // Rename rather than write in place: a half-written record read by
      // another window would be a parse error every time until it expired.
      const staged = `${target}.${randomUUID()}.tmp`;
      await writeFile(staged, JSON.stringify(record), "utf8");
      await rename(staged, target);
    } catch {
      // A cache that cannot be written is slower, not broken.
    }
  }

  /**
   * One request, redirects followed by hand so every hop is re-validated.
   *
   * Only `status`, the `location` header and `body` are read. Electron's own
   * documentation warns that `net.fetch` returns INCORRECT `.type` and `.url`
   * on its `Response`, so a "follow the redirects and check where we ended
   * up" design would be checking a value Electron admits is wrong. Reading
   * the location header and re-validating each hop is the only shape that is
   * true on both Chromium's stack and Node's.
   *
   * Where a runtime declines to hand over the redirect at all, the icon is
   * simply not fetched — an unfollowable redirect is a `null`, and a `null`
   * is the globe. Failing closed is the right default for the one thing in
   * this room that dials out.
   */
  async function fetchIcon(host) {
    let url = `https://${host}/favicon.ico`;
    for (let hop = 0; hop <= MAX_REDIRECTS; hop += 1) {
      let response;
      try {
        response = await fetchImpl(url, {
          method: "GET",
          redirect: "manual",
          credentials: "omit",
          referrerPolicy: "no-referrer",
          cache: "no-store",
          headers: { Accept: "image/*" },
          signal: AbortSignal.timeout(FETCH_TIMEOUT_MS),
        });
      } catch {
        return null;
      }

      const status = response?.status ?? 0;
      if (status >= 300 && status < 400) {
        const location = response.headers?.get?.("location");
        if (typeof location !== "string" || location === "") return null;
        let next;
        try {
          next = new URL(location, url);
        } catch {
          return null;
        }
        // A redirect is the site asking this process to dial somewhere else,
        // which is exactly the request the validator exists to answer. `http`
        // is refused here too: an icon is not worth a downgrade.
        if (next.protocol !== "https:") return null;
        if (normalizeHost(next.hostname) === null) return null;
        url = next.toString();
        continue;
      }

      if (status !== 200) return null;
      const bytes = await readCapped(response.body, MAX_ICON_BYTES);
      if (bytes === null || bytes.length === 0) return null;
      const mime = sniffImage(bytes);
      if (mime === null) return null;
      return toDataUrl(mime, bytes);
    }
    return null;
  }

  async function resolve(host) {
    const onDisk = await readRecord(host);
    if (fresh(onDisk)) {
      memory.set(host, onDisk);
      return onDisk.dataUrl;
    }
    const dataUrl = await fetchIcon(host);
    const record = { host, fetchedAt: now(), dataUrl };
    memory.set(host, record);
    await writeRecord(record);
    return dataUrl;
  }

  return {
    /** A `data:` URL for this host's icon, or `null` for every honest no. */
    async get(raw) {
      const host = normalizeHost(raw);
      if (host === null) return null;

      const remembered = memory.get(host);
      if (fresh(remembered)) return remembered.dataUrl;
      // Stale in memory is stale everywhere; drop it before re-reading.
      if (remembered !== undefined) memory.delete(host);

      const pending = inFlight.get(host);
      if (pending !== undefined) return pending;

      const work = resolve(host).finally(() => inFlight.delete(host));
      inFlight.set(host, work);
      return work;
    },

    /** Test and diagnostic seam; the app never needs it. */
    forget() {
      memory.clear();
    },
  };
}
