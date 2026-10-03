import type { FileHandle, FileSource } from "./types";

/** Share thumbnail/main-image reads for one open gallery. Released on close;
 * the byte cache is bounded even when a row contains many large images. */
export function createGallerySource(source: FileSource, limit = 64 * 1024 * 1024) {
  const cache = new Map<string, Uint8Array>();
  let size = 0;
  type Pending = { controller: AbortController; promise: Promise<Uint8Array>; readers: number; done: boolean };
  const pending = new Map<string, Pending>();
  const forget = (key: string) => {
    size -= cache.get(key)?.byteLength ?? 0;
    cache.delete(key);
  };
  const read = (handle: FileHandle, signal: AbortSignal): Promise<Uint8Array> => {
    const cancelled = () => new DOMException("Image read cancelled", "AbortError");
    if (signal.aborted) return Promise.reject(cancelled());
    const cached = cache.get(handle.key);
    if (cached) {
      cache.delete(handle.key); cache.set(handle.key, cached);
      return Promise.resolve(cached);
    }
    let request = pending.get(handle.key);
    if (!request) {
      const controller = new AbortController();
      const created: Pending = { controller, readers: 0, done: false, promise: Promise.resolve(new Uint8Array()) };
      created.promise = Promise.resolve().then(() => source.read(handle, controller.signal)).then(bytes => {
        if (!controller.signal.aborted && bytes.byteLength <= limit) {
          forget(handle.key);
          while (size + bytes.byteLength > limit && cache.size) forget(cache.keys().next().value!);
          cache.set(handle.key, bytes); size += bytes.byteLength;
        }
        return bytes;
      }).finally(() => {
        created.done = true;
        if (pending.get(handle.key) === created) pending.delete(handle.key);
      });
      request = created;
      pending.set(handle.key, created);
    }
    const active = request;
    active.readers += 1;
    return new Promise((resolve, reject) => {
      let finished = false;
      const finish = (result: () => void) => {
        if (finished) return;
        finished = true;
        signal.removeEventListener("abort", abort);
        active.readers -= 1;
        if (active.readers === 0 && !active.done) {
          active.controller.abort();
          if (pending.get(handle.key) === active) pending.delete(handle.key);
        }
        result();
      };
      const abort = () => finish(() => reject(cancelled()));
      signal.addEventListener("abort", abort, { once: true });
      active.promise.then(bytes => finish(() => resolve(bytes)), error => finish(() => reject(error)));
    });
  };
  return {
    source: { read, image: source.image?.bind(source) } satisfies FileSource,
    invalidate: (key: string) => forget(key),
    dispose: () => {
      for (const request of pending.values()) request.controller.abort();
      pending.clear(); cache.clear(); size = 0;
    },
  };
}
