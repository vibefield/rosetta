import assert from 'node:assert/strict';
import { test } from 'node:test';
import { RosettaClient } from '../../ts/client.ts';

const client = new RosettaClient({ baseUrl: 'http://127.0.0.1:4640' });
const asset = (digest, image_variants) => ({ digest, url: `/api/assets/${digest}.webp`, media_type: 'image/webp', byte_len: 100, image_variants });
const RENDITIONS = ['rail', 'tile', 'tile2x', 'preview', 'full'];
const FORMATS = [['image/png', 'png'], ['image/jpeg', 'jpg'], ['image/gif', 'gif'], ['image/webp', 'webp'], ['image/bmp', 'bmp'], ['image/x-icon', 'ico']];
const original = (media_type, extension, digest = '1'.repeat(64)) => ({ url: `/api/assets/${digest}.${extension}`, digest, media_type, byte_len: 4096 });

test('each image rendition uses its own immutable identity', () => {
  const first = asset('same-full', { tile: asset('first-tile') });
  const second = asset('same-full', { tile: asset('different-tile') });
  assert.equal(client.imageAssetUrl(first, 'full'), client.imageAssetUrl(second, 'full'));
  assert.equal(client.imageAssetUrl(first, 'tile'), 'http://127.0.0.1:4640/api/assets/first-tile.webp');
  assert.notEqual(client.imageAssetUrl(first, 'tile'), client.imageAssetUrl(second, 'tile'));
  assert.equal(client.imageAssetUrl(first, 'rail'), 'http://127.0.0.1:4640/api/assets/same-full.webp?image=rail&v=1');
  assert.throws(() => client.imageAssetUrl({ ...first, image_variants: { tile: { ...asset('x'), url: 'https://example.com/x.webp' } } }, 'tile'), /unsupported address/);
});

test('a legacy WebP bundle keeps exactly its own member addresses', () => {
  const bundle = asset('a'.repeat(64), { rail: asset('b'.repeat(64)), tile: asset('c'.repeat(64)), tile2x: asset('d'.repeat(64)), preview: asset('e'.repeat(64)) });
  assert.deepEqual(RENDITIONS.map(variant => client.imageAssetUrl(bundle, variant)), [
    `http://127.0.0.1:4640/api/assets/${'b'.repeat(64)}.webp`,
    `http://127.0.0.1:4640/api/assets/${'c'.repeat(64)}.webp`,
    `http://127.0.0.1:4640/api/assets/${'d'.repeat(64)}.webp`,
    `http://127.0.0.1:4640/api/assets/${'e'.repeat(64)}.webp`,
    `http://127.0.0.1:4640/api/assets/${'a'.repeat(64)}.webp`,
  ]);
});

test('a recorded original is drawn only through daemon renditions, full included', () => {
  for (const [mediaType, extension] of FORMATS) {
    const recorded = original(mediaType, extension);
    for (const variant of RENDITIONS) {
      const url = new URL(client.imageAssetUrl(recorded, variant));
      assert.equal(url.pathname, recorded.url, `${mediaType} ${variant} names its member`);
      assert.deepEqual([...url.searchParams], [['image', variant], ['v', '1']], `${mediaType} ${variant} is a rendition`);
    }
  }
  // The same digest grammar names a legacy bundle's full WebP and a recorded
  // WebP original. Only the bundle's own member is shown as it is.
  const webp = original('image/webp', 'webp');
  assert.equal(client.imageAssetUrl(webp, 'full'), `http://127.0.0.1:4640${webp.url}?image=full&v=1`);
  assert.equal(client.imageAssetUrl({ ...webp, image_variants: {} }, 'full'), `http://127.0.0.1:4640${webp.url}?image=full&v=1`,
    'an empty rendition map is no bundle');
  assert.equal(client.imageAssetUrl({ ...original('image/png', 'png'), image_variants: { tile: asset('c'.repeat(64)) } }, 'full'),
    `http://127.0.0.1:4640/api/assets/${'1'.repeat(64)}.png?image=full&v=1`, 'a bundle is WebP: a PNG member is never shown as it is');
  assert.equal(client.imageAssetUrl(asset('a'.repeat(64), { tile: original('image/png', 'png') }), 'tile'),
    `http://127.0.0.1:4640/api/assets/${'a'.repeat(64)}.webp?image=tile&v=1`, 'nor is a PNG leaf');
  assert.throws(() => client.imageAssetUrl({ ...webp, url: 'https://example.com/x.png' }, 'full'), /unsupported address/);
});

test('tool-image URLs contain exact canonical identities and finite recipe parameters', () => {
  assert.equal(client.imageFileTargetUrl('session/one', 'image?one', 2, 'preview'),
    'http://127.0.0.1:4640/api/sessions/session%2Fone/timeline/image%3Fone/read/2?image=preview&v=1');
  for (const index of [-1, 0.5, Number.MAX_SAFE_INTEGER + 1])
    assert.throws(() => client.imageFileTargetUrl('s', 't', index, 'full'), /invalid image target/);
});
