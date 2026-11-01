// Disposable PNGs only; captured HTML stays in Rosetta's archive. Rendering is
// serialized and content-addressed so opening a large library starts one job.
import { createHash } from "node:crypto";
import { mkdir, readFile, readdir, stat, unlink, writeFile } from "node:fs/promises";
import path from "node:path";

const MAX_HTML = 16 * 1024 * 1024 + 16 * 1024;
const MAX_PNG = 2 * 1024 * 1024;
const PNG_SIGNATURE = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
const validPng = (bytes) => Buffer.isBuffer(bytes) && bytes.length <= MAX_PNG && bytes.subarray(0, 8).equals(PNG_SIGNATURE);

export function createArtifactThumbnailService({ cacheDir, render, maxCacheBytes = 32 * 1024 * 1024, maxEntries = 256 }) {
  let tail = Promise.resolve(), queuedBytes = 0, stopped = false;
  const pending = new Map();

  async function prune() {
    const entries = await Promise.all((await readdir(cacheDir)).filter((name) => /^v1-[a-f0-9]{64}\.png$/.test(name)).map(async (name) => {
      const file = path.join(cacheDir, name);
      const info = await stat(file).catch(() => null);
      return { file, bytes: info?.size ?? 0, at: info?.mtimeMs ?? 0 };
    }));
    entries.sort((a, b) => a.at - b.at);
    let bytes = entries.reduce((sum, entry) => sum + entry.bytes, 0), count = entries.length;
    for (const entry of entries) {
      if (bytes <= maxCacheBytes && count <= maxEntries) break;
      await unlink(entry.file).catch(() => {});
      bytes -= entry.bytes; count--;
    }
  }

  function get(html) {
    if (stopped || typeof html !== "string" || html.length === 0 || html.length > MAX_HTML) return Promise.resolve(null);
    const size = Buffer.byteLength(html);
    if (size > MAX_HTML) return Promise.resolve(null);
    const key = createHash("sha256").update(html).digest("hex");
    if (pending.has(key)) return pending.get(key);
    if (pending.size >= 16 || queuedBytes + size > 32 * 1024 * 1024) return Promise.resolve(null);
    queuedBytes += size;
    const task = tail.then(async () => {
      if (stopped) return null;
      const file = path.join(cacheDir, `v1-${key}.png`);
      const info = await stat(file).catch(() => null);
      if (info?.isFile() && info.size <= MAX_PNG) {
        const bytes = await readFile(file).catch(() => null);
        if (validPng(bytes)) return `data:image/png;base64,${bytes.toString("base64")}`;
      }
      const bytes = await render(html);
      if (!validPng(bytes) || stopped) return null;
      // A read-only/full cache must not prevent an otherwise valid thumbnail.
      await mkdir(cacheDir, { recursive: true }).then(async () => {
        await writeFile(file, bytes, { mode: 0o600 });
        await prune();
      }).catch(() => {});
      return `data:image/png;base64,${bytes.toString("base64")}`;
    }).catch(() => null).finally(() => { pending.delete(key); queuedBytes -= size; });
    pending.set(key, task);
    tail = task;
    return task;
  }
  return { get, dispose() { stopped = true; } };
}
