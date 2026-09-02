// The favicon service, proved without a network.
//
// Every test here drives `createFaviconService` with an injected fetch and an
// injected clock, so the suite is hermetic: no live host is dialled, no real
// icon is depended on, and a TTL that takes a month in the world takes a line
// here. The one thing a fake fetch cannot prove is that the real one works,
// which is why the validator — the half that decides whether a request
// happens at all — is tested directly rather than through it.

import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { test } from "node:test";
import {
  MAX_ICON_BYTES,
  NEGATIVE_TTL_MS,
  POSITIVE_TTL_MS,
  createFaviconService,
  hostOfUrl,
  normalizeHost,
  sniffImage,
} from "../electron/favicon-service.mjs";

const PNG = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13]);
const ICO = new Uint8Array([0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x10, 0x10]);
const GIF = new Uint8Array([0x47, 0x49, 0x46, 0x38, 0x39, 0x61]);
const JPEG = new Uint8Array([0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10]);
const WEBP = new Uint8Array([
  0x52, 0x49, 0x46, 0x46, 0x20, 0x00, 0x00, 0x00, 0x57, 0x45, 0x42, 0x50,
]);
const SVG = new TextEncoder().encode('<svg xmlns="http://www.w3.org/2000/svg"></svg>');
const HTML = new TextEncoder().encode("<!doctype html><html><body>not found</body></html>");
/** A cursor file. Its header is one byte from an ICO's, which is exactly why
 * a sniffer that only checked two bytes would admit it. */
const CUR = new Uint8Array([0x00, 0x00, 0x02, 0x00, 0x01, 0x00]);

function streamOf(bytes, chunk = bytes.byteLength) {
  let at = 0;
  return new ReadableStream({
    pull(controller) {
      if (at >= bytes.byteLength) {
        controller.close();
        return;
      }
      controller.enqueue(bytes.subarray(at, at + chunk));
      at += chunk;
    },
  });
}

function reply(status, bytes, headers = {}) {
  const lower = new Map(Object.entries(headers).map(([k, v]) => [k.toLowerCase(), v]));
  return {
    status,
    headers: { get: (key) => lower.get(String(key).toLowerCase()) ?? null },
    body: bytes === null ? null : streamOf(bytes),
  };
}

/** A fetch that answers from a table and records every url it was asked for. */
function recordingFetch(answer) {
  const calls = [];
  const impl = async (url, init) => {
    calls.push(url);
    const result = await answer(url, init, calls.length);
    if (result instanceof Error) throw result;
    return result;
  };
  impl.calls = calls;
  return impl;
}

async function withCacheDir(body) {
  const dir = await mkdtemp(path.join(tmpdir(), "rosetta-favicon-"));
  try {
    return await body(dir);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
}

// ── the validator: which hosts this process will dial ─────────────────────

test("a public domain name is accepted, in the form that would be dialled", () => {
  assert.equal(normalizeHost("developer.mozilla.org"), "developer.mozilla.org");
  assert.equal(normalizeHost("github.com"), "github.com");
  assert.equal(normalizeHost("toji.dev"), "toji.dev");
  assert.equal(normalizeHost("w3.org"), "w3.org");
  // Case and a trailing dot name the same host; both normalize.
  assert.equal(normalizeHost("GitHub.COM"), "github.com");
  assert.equal(normalizeHost("github.com."), "github.com");
  // Surrounding whitespace is noise around the same name, so it is trimmed.
  // Whitespace INSIDE one is not — see the refusals below — because `URL`
  // would silently delete it and hand back a host nobody wrote.
  assert.equal(normalizeHost("  github.com  "), "github.com");
  assert.equal(normalizeHost("github.com\n"), "github.com");
  // A unicode host is dialled in punycode, so that is what is returned and
  // what the cache is keyed by.
  assert.equal(normalizeHost("bücher.de"), "xn--bcher-kva.de");
  assert.equal(normalizeHost("münchen.example.org"), "xn--mnchen-3ya.example.org");
});

test("everything that is not a bare public host is refused", () => {
  for (const raw of [
    // the machine itself, and the network it sits on
    "localhost",
    "LOCALHOST",
    "sub.localhost",
    "127.0.0.1",
    "10.0.0.1",
    "192.168.1.1",
    "169.254.169.254",
    "0.0.0.0",
    "[::1]",
    "::1",
    "[fd00::1]",
    "printer.local",
    "db.internal",
    "1.0.0.127.in-addr.arpa",
    // more than a hostname was handed over
    "evil.com/path",
    "evil.com/../../etc/passwd",
    "user@host.com",
    "user:pw@host.com",
    "host.com:8080",
    "https://github.com",
    "github.com?q=1",
    "github.com#frag",
    "github.com\\evil.com",
    "git hub.com",
    "git\nhub.com",
    "git\thub.com",
    // not a name at all
    "",
    "   ",
    ".",
    "..",
    "com",
    "-github.com",
    "github-.com",
    "github..com",
    "8.8.8.8",
    // reserved, unroutable, or not-a-real-TLD
    "foo.test",
    "foo.invalid",
    "example.onion",
    // over the DNS limits
    `${"a".repeat(64)}.com`,
    `${`${"a".repeat(60)}.`.repeat(5)}com`,
    // not a string
    null,
    undefined,
    42,
    {},
    ["github.com"],
  ]) {
    assert.equal(normalizeHost(raw), null, `must refuse ${JSON.stringify(raw)}`);
  }
});

test("a source url yields the host, or nothing", () => {
  assert.equal(hostOfUrl("https://developer.mozilla.org/en-US/docs/Web/API"), "developer.mozilla.org");
  assert.equal(hostOfUrl("http://toji.dev/webgpu-best-practices"), "toji.dev");
  assert.equal(hostOfUrl("https://GitHub.com:443/gpuweb"), "github.com");
  assert.equal(hostOfUrl("https://user:pw@github.com/x"), "github.com");
  assert.equal(hostOfUrl("file:///etc/passwd"), null);
  assert.equal(hostOfUrl("http://localhost:5185/"), null);
  assert.equal(hostOfUrl("not a url"), null);
  assert.equal(hostOfUrl(""), null);
});

// ── the sniffer: what the bytes actually are ──────────────────────────────

test("an image is recognised by its bytes, never by a header", () => {
  assert.equal(sniffImage(ICO), "image/x-icon");
  assert.equal(sniffImage(PNG), "image/png");
  assert.equal(sniffImage(GIF), "image/gif");
  assert.equal(sniffImage(JPEG), "image/jpeg");
  assert.equal(sniffImage(WEBP), "image/webp");
  assert.equal(sniffImage(SVG), "image/svg+xml");
  assert.equal(
    sniffImage(new TextEncoder().encode('<?xml version="1.0"?><svg viewBox="0 0 16 16"/>')),
    "image/svg+xml",
  );
});

test("anything that is not an image is refused, however it was labelled", () => {
  assert.equal(sniffImage(HTML), null);
  assert.equal(sniffImage(CUR), null);
  assert.equal(sniffImage(new TextEncoder().encode("<html><svg></svg></html>")), null);
  assert.equal(sniffImage(new Uint8Array([0x89, 0x50])), null);
  assert.equal(sniffImage(new Uint8Array(0)), null);
  assert.equal(sniffImage(new Uint8Array([0x52, 0x49, 0x46, 0x46])), null);
  assert.equal(sniffImage("not bytes"), null);
  assert.equal(sniffImage(null), null);
});

// ── the service ───────────────────────────────────────────────────────────

test("an icon is fetched from the site itself and returned as data", async () => {
  await withCacheDir(async (cacheDir) => {
    const fetchImpl = recordingFetch(() => reply(200, PNG, { "content-type": "text/plain" }));
    const service = createFaviconService({ cacheDir, fetch: fetchImpl });

    const src = await service.get("github.com");
    assert.equal(typeof src, "string");
    assert.ok(src.startsWith("data:image/png;base64,"));
    // The site itself. Never a favicon service.
    assert.deepEqual(fetchImpl.calls, ["https://github.com/favicon.ico"]);
  });
});

test("a refused host is never dialled at all", async () => {
  await withCacheDir(async (cacheDir) => {
    const fetchImpl = recordingFetch(() => reply(200, PNG));
    const service = createFaviconService({ cacheDir, fetch: fetchImpl });

    for (const raw of ["localhost", "127.0.0.1", "evil.com/path", "", null]) {
      assert.equal(await service.get(raw), null);
    }
    assert.deepEqual(fetchImpl.calls, []);
  });
});

test("a 404, a non-image body and an oversized body are all the same honest no", async () => {
  await withCacheDir(async (cacheDir) => {
    const big = new Uint8Array(MAX_ICON_BYTES + 4096);
    big.set(PNG, 0);
    const answers = new Map([
      ["https://a.example.org/favicon.ico", () => reply(404, HTML)],
      ["https://b.example.org/favicon.ico", () => reply(200, HTML, { "content-type": "image/png" })],
      ["https://c.example.org/favicon.ico", () => reply(200, big)],
      ["https://d.example.org/favicon.ico", () => reply(200, null)],
      ["https://e.example.org/favicon.ico", () => new Error("ETIMEDOUT")],
      ["https://f.example.org/favicon.ico", () => reply(500, HTML)],
    ]);
    const fetchImpl = recordingFetch((url) => answers.get(url)());
    const service = createFaviconService({ cacheDir, fetch: fetchImpl });

    for (const host of ["a", "b", "c", "d", "e", "f"].map((n) => `${n}.example.org`)) {
      assert.equal(await service.get(host), null, `${host} must answer null`);
    }
    assert.equal(fetchImpl.calls.length, 6);
  });
});

test("a redirect is followed only to a host this process would have dialled", async () => {
  await withCacheDir(async (cacheDir) => {
    const answers = new Map([
      // an ordinary cross-host redirect, re-validated and followed
      [
        "https://good.example.org/favicon.ico",
        () => reply(301, null, { location: "https://cdn.example.net/icon.png" }),
      ],
      ["https://cdn.example.net/icon.png", () => reply(200, PNG)],
      // the same shape, pointed at the machine's own metadata service
      [
        "https://ssrf.example.org/favicon.ico",
        () => reply(302, null, { location: "https://169.254.169.254/latest/meta-data/" }),
      ],
      // and a downgrade to plain http
      [
        "https://downgrade.example.org/favicon.ico",
        () => reply(302, null, { location: "http://downgrade.example.org/icon.png" }),
      ],
      // a redirect with nowhere to go
      ["https://empty.example.org/favicon.ico", () => reply(302, null)],
    ]);
    const fetchImpl = recordingFetch((url) => answers.get(url)());
    const service = createFaviconService({ cacheDir, fetch: fetchImpl });

    assert.ok((await service.get("good.example.org"))?.startsWith("data:image/png;"));
    assert.equal(await service.get("ssrf.example.org"), null);
    assert.equal(await service.get("downgrade.example.org"), null);
    assert.equal(await service.get("empty.example.org"), null);
    // Two calls for the followed redirect, one each for the three refused.
    assert.deepEqual(fetchImpl.calls, [
      "https://good.example.org/favicon.ico",
      "https://cdn.example.net/icon.png",
      "https://ssrf.example.org/favicon.ico",
      "https://downgrade.example.org/favicon.ico",
      "https://empty.example.org/favicon.ico",
    ]);
  });
});

test("a redirect loop terminates", async () => {
  await withCacheDir(async (cacheDir) => {
    const fetchImpl = recordingFetch((url) =>
      reply(302, null, { location: url.includes("a.") ? "https://b.example.org/x" : "https://a.example.org/x" }),
    );
    const service = createFaviconService({ cacheDir, fetch: fetchImpl });
    assert.equal(await service.get("a.example.org"), null);
    assert.ok(fetchImpl.calls.length <= 4, "a loop must not be followed forever");
  });
});

test("a host is dialled once, then answered from cache", async () => {
  await withCacheDir(async (cacheDir) => {
    const fetchImpl = recordingFetch(() => reply(200, PNG));
    const service = createFaviconService({ cacheDir, fetch: fetchImpl });

    const first = await service.get("github.com");
    const second = await service.get("github.com");
    // and again for a host that only differs by the normalization
    const third = await service.get("GitHub.com.");
    assert.equal(first, second);
    assert.equal(first, third);
    assert.equal(fetchImpl.calls.length, 1);
  });
});

test("the cache outlives the process", async () => {
  await withCacheDir(async (cacheDir) => {
    const first = recordingFetch(() => reply(200, ICO));
    const src = await createFaviconService({ cacheDir, fetch: first }).get("github.com");
    assert.ok(src.startsWith("data:image/x-icon;base64,"));

    // A second service — a relaunched app — reads the record from disk.
    const second = recordingFetch(() => reply(200, PNG));
    const again = await createFaviconService({ cacheDir, fetch: second }).get("github.com");
    assert.equal(again, src);
    assert.deepEqual(second.calls, []);
  });
});

test("a record is re-fetched once its TTL is spent", async () => {
  await withCacheDir(async (cacheDir) => {
    let clock = 1_756_000_000_000;
    const fetchImpl = recordingFetch(() => reply(200, PNG));
    const service = createFaviconService({ cacheDir, fetch: fetchImpl, now: () => clock });

    await service.get("github.com");
    assert.equal(fetchImpl.calls.length, 1);

    clock += POSITIVE_TTL_MS - 1000;
    await service.get("github.com");
    assert.equal(fetchImpl.calls.length, 1, "still fresh a moment before expiry");

    clock += 2000;
    await service.get("github.com");
    assert.equal(fetchImpl.calls.length, 2, "stale after expiry");
  });
});

test("a refusal is cached too, on a shorter leash", async () => {
  await withCacheDir(async (cacheDir) => {
    let clock = 1_756_000_000_000;
    const fetchImpl = recordingFetch(() => reply(404, HTML));
    const service = createFaviconService({ cacheDir, fetch: fetchImpl, now: () => clock });

    assert.equal(await service.get("toji.dev"), null);
    assert.equal(fetchImpl.calls.length, 1);

    // A site with no icon must not be re-dialled on every scroll.
    for (let i = 0; i < 5; i += 1) assert.equal(await service.get("toji.dev"), null);
    assert.equal(fetchImpl.calls.length, 1);

    clock += NEGATIVE_TTL_MS - 1000;
    await service.get("toji.dev");
    assert.equal(fetchImpl.calls.length, 1, "still remembered a moment before expiry");

    // But a day later the answer may have changed, and it is asked again.
    clock += 2000;
    await service.get("toji.dev");
    assert.equal(fetchImpl.calls.length, 2);
    // A negative record expires on the SHORT leash, not the long one.
    assert.ok(NEGATIVE_TTL_MS < POSITIVE_TTL_MS);
  });
});

test("a record written in the future is not trusted as fresh", async () => {
  await withCacheDir(async (cacheDir) => {
    let clock = 1_756_000_000_000;
    const fetchImpl = recordingFetch(() => reply(200, PNG));
    const service = createFaviconService({ cacheDir, fetch: fetchImpl, now: () => clock });

    await service.get("github.com");
    assert.equal(fetchImpl.calls.length, 1);
    // The clock moved back — a timezone fix, an NTP correction, a reboot.
    clock -= 60 * 60 * 1000;
    await service.get("github.com");
    assert.equal(fetchImpl.calls.length, 2);
  });
});

test("eight lines from one site dial it once", async () => {
  await withCacheDir(async (cacheDir) => {
    const fetchImpl = recordingFetch(() => reply(200, PNG));
    const service = createFaviconService({ cacheDir, fetch: fetchImpl });

    // Every line in a ledger asks in the same tick, before any answer lands.
    // None of the eight can be served from a settled cache, so without
    // coalescing this is eight requests to one site for one 14px icon.
    const asked = Array.from({ length: 8 }, () => service.get("developer.mozilla.org"));
    const results = await Promise.all(asked);
    assert.equal(fetchImpl.calls.length, 1, "the request must not be made eight times");
    assert.equal(new Set(results).size, 1);
    assert.ok(results[0].startsWith("data:image/png;base64,"));
  });
});

test("different hosts asked together are each dialled once", async () => {
  await withCacheDir(async (cacheDir) => {
    const fetchImpl = recordingFetch(() => reply(200, GIF));
    const service = createFaviconService({ cacheDir, fetch: fetchImpl });
    const hosts = ["github.com", "developer.mozilla.org", "stackoverflow.com", "w3.org"];
    const results = await Promise.all(hosts.flatMap((h) => [service.get(h), service.get(h)]));
    assert.equal(fetchImpl.calls.length, 4);
    assert.ok(results.every((src) => src.startsWith("data:image/gif;base64,")));
  });
});

test("a failed request does not wedge the host forever", async () => {
  await withCacheDir(async (cacheDir) => {
    let clock = 1_756_000_000_000;
    let attempt = 0;
    const fetchImpl = recordingFetch(() => {
      attempt += 1;
      return attempt === 1 ? new Error("ECONNRESET") : reply(200, PNG);
    });
    const service = createFaviconService({ cacheDir, fetch: fetchImpl, now: () => clock });

    assert.equal(await service.get("github.com"), null);
    clock += NEGATIVE_TTL_MS + 1000;
    assert.ok((await service.get("github.com"))?.startsWith("data:image/png;"));
  });
});
