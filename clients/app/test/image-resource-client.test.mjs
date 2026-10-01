import assert from 'node:assert/strict';
import { test } from 'node:test';
import { RosettaClient } from '../../ts/client.ts';

const client = new RosettaClient({ baseUrl: 'http://127.0.0.1:4640' });
const asset = (digest, image_variants) => ({ digest, url: `/api/assets/${digest}.webp`, media_type: 'image/webp', byte_len: 100, image_variants });

test('each image rendition uses its own immutable identity', () => {
  const first = asset('same-full', { tile: asset('first-tile') });
  const second = asset('same-full', { tile: asset('different-tile') });
  assert.equal(client.imageAssetUrl(first, 'full'), client.imageAssetUrl(second, 'full'));
  assert.equal(client.imageAssetUrl(first, 'tile'), 'http://127.0.0.1:4640/api/assets/first-tile.webp');
  assert.notEqual(client.imageAssetUrl(first, 'tile'), client.imageAssetUrl(second, 'tile'));
  assert.equal(client.imageAssetUrl(first, 'rail'), 'http://127.0.0.1:4640/api/assets/same-full.webp?image=rail&v=1');
  assert.throws(() => client.imageAssetUrl({ ...first, image_variants: { tile: { ...asset('x'), url: 'https://example.com/x.webp' } } }, 'tile'), /unsupported address/);
});

test('tool-image URLs contain exact canonical identities and finite recipe parameters', () => {
  assert.equal(client.imageFileTargetUrl('session/one', 'image?one', 2, 'preview'),
    'http://127.0.0.1:4640/api/sessions/session%2Fone/timeline/image%3Fone/read/2?image=preview&v=1');
  for (const index of [-1, 0.5, Number.MAX_SAFE_INTEGER + 1])
    assert.throws(() => client.imageFileTargetUrl('s', 't', index, 'full'), /invalid image target/);
});
