import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createGallerySource } from '../renderer/src/components/file-viewer/gallery-source.ts';

const handle = key => ({ key, name: 'image.png', path: null, size: 4 });
const deferred = () => { let resolve; const promise = new Promise(done => { resolve = done; }); return { promise, resolve }; };

test('HTTP image resources pass through the gallery without reading byte buffers', () => {
  let reads = 0;
  const urls = { key: 'one', url: rendition => `/${rendition}.webp` };
  const source = { image: () => urls, read: async () => { reads++; return new Uint8Array(0); } };
  const gallery = createGallerySource(source);
  assert.equal(gallery.source.image(handle('one')), urls);
  assert.equal(reads, 0);
  gallery.dispose();
});

test('thumbnail and main image share one read, even when the main image moves on', async () => {
  const loaded = deferred();
  const calls = [];
  const gallery = createGallerySource({ read: (file, signal) => { calls.push({ file, signal }); return loaded.promise; } });
  const thumbnail = new AbortController(), main = new AbortController();
  const first = gallery.source.read(handle('one'), thumbnail.signal);
  const second = gallery.source.read(handle('one'), main.signal);
  main.abort();
  await assert.rejects(second, { name: 'AbortError' });
  assert.equal(calls.length, 1);
  assert.equal(calls[0].signal.aborted, false);
  loaded.resolve(new Uint8Array([1, 2, 3, 4]));
  assert.deepEqual(await first, new Uint8Array([1, 2, 3, 4]));
  await gallery.source.read(handle('one'), new AbortController().signal);
  assert.equal(calls.length, 1, 'Returning to this image reuses the same bytes');
  gallery.dispose();
});

test('cancelled or closed reads cannot replace a newer image snapshot', async () => {
  const old = deferred(), current = deferred();
  const calls = [];
  const gallery = createGallerySource({ read: (_file, signal) => {
    calls.push(signal); return calls.length === 1 ? old.promise : current.promise;
  } });
  const controller = new AbortController();
  const stale = gallery.source.read(handle('one'), controller.signal);
  await Promise.resolve(); controller.abort();
  await assert.rejects(stale, { name: 'AbortError' });
  assert.ok(calls[0].aborted);
  const fresh = gallery.source.read(handle('one'), new AbortController().signal);
  current.resolve(new Uint8Array([2]));
  assert.deepEqual(await fresh, new Uint8Array([2]));
  old.resolve(new Uint8Array([1]));
  await Promise.resolve(); await Promise.resolve();
  assert.deepEqual(await gallery.source.read(handle('one'), new AbortController().signal), new Uint8Array([2]));
  gallery.dispose();
});

test('failed reads retry and retained bytes stay within the gallery budget', async () => {
  let reads = 0;
  const gallery = createGallerySource({ read: async () => {
    if (++reads === 1) throw new Error('temporary failure');
    return new Uint8Array([reads, 0, 0, 0]);
  } }, 4);
  const read = key => gallery.source.read(handle(key), new AbortController().signal);
  await assert.rejects(read('one'), /temporary failure/);
  assert.equal((await read('one'))[0], 2);
  await read('two');
  assert.equal((await read('one'))[0], 4, 'The older image is reread after eviction');
  gallery.invalidate('one');
  assert.equal((await read('one'))[0], 5, 'Explicit retry releases a failed decode snapshot');
  gallery.dispose();
});
