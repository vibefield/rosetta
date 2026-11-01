import assert from 'node:assert/strict';
import { test } from 'node:test';
import { imageMediaTypeFromBytes, isLegacyImageBundle, isRecordedImageAsset } from '../../room/src/lib/image-media.ts';
import { activityImages, messageAttachments, timelineImages } from '../../room/src/lib/transcript/images.ts';
import { ledgerLine } from '../../room/src/lib/transcript/index.ts';
import { entry, tool, read, file, inline } from './support/ledger-fixtures.mjs';

const images = (...rows) => activityImages(rows.map(row => ({ kind: 'line', line: ledgerLine(row) })));
const member = digit => ({ url: `/api/assets/${digit.repeat(64)}.webp`, digest: digit.repeat(64), media_type: 'image/webp', byte_len: 42 });
const optimized = () => ({ ...member('a'), image_variants: { rail: member('b'), tile: member('c'), tile2x: member('d'), preview: member('e') } });
const asset = { type: 'asset', asset: optimized() };
const FORMATS = [['image/png', 'png'], ['image/jpeg', 'jpg'], ['image/gif', 'gif'], ['image/webp', 'webp'], ['image/bmp', 'bmp'], ['image/x-icon', 'ico']];
const original = (media_type = 'image/png', extension = 'png', digit = '1') =>
  ({ url: `/api/assets/${digit.repeat(64)}.${extension}`, digest: digit.repeat(64), media_type, byte_len: 4096 });
const attachment = (over = {}) => ({ id: '1', kind: 'image', label: '[Image #1]', name: 'review.png',
  media_type: 'image/webp', byte_len: 42, dimensions: null, message_ranges: [], source: asset, ...over });

test('mixed activity gathers recorded tool, assignment, team and peer images in message order', () => {
  const assignment = entry({ type: 'user', text: inline('[Image #1]'), attachments: [attachment()] });
  const team = entry({ type: 'collaboration', event: { channel: 'team', attachments: [attachment()] } });
  const peer = entry({ type: 'collaboration', event: { channel: 'peer', attachments: [attachment({ source: { type: 'unavailable', reason: 'not_recorded' } })] } });
  const report = entry({ type: 'collaboration', event: { kind: 'report', attachments: [attachment()] } });
  const readRow = read('/already/deleted.png', { attachments: [attachment()] });
  const result = timelineImages([assignment, readRow, team, peer, report, team]);
  assert.equal(result.length, 5, 'a repeated canonical row is not a second occurrence');
  assert.deepEqual(result.map(fact => fact.key), [messageAttachments(assignment)[0].key, images(readRow)[0].key,
    messageAttachments(team)[0].key, messageAttachments(peer)[0].key, messageAttachments(report)[0].key]);
  assert.equal(new Set(result.map(fact => fact.key)).size, 5, 'message-local ids must not collide');
  assert.equal(result[3].missing, true);
  assert.equal(result[2].ref, team.body.event.attachments[0]);
});

test('filenames, declared media kinds, remote URLs and markdown never invent transcript images', () => {
  assert.deepEqual(timelineImages([
    read('/demo/screen.png'),
    tool({ type: 'file_read', targets: [{ resource: file('/demo/no-extension'), range: null, media_kind: 'image' }], outcome: null }),
    tool({ type: 'artifact', operation: 'create', artifact_kind: 'image', name: 'Design', description: null,
      resources: [file('/demo/design.png'), { type: 'url', url: 'https://example.test/design.png' }], media: null }),
    entry({ type: 'collaboration', event: { body: inline('![image](https://example.test/a.png)') } }),
    entry({ type: 'summary', text: inline('/demo/screen.png') }),
    entry({ type: 'user', attachments: [attachment({ kind: 'document' })] }),
  ]), []);
});

test('recorded occurrences survive repeated bytes and successive tools without live read addresses', () => {
  const first = read('/gone.png', { attachments: [attachment(), attachment({ id: '2' })] });
  const second = read('/gone.png', { attachments: [attachment()] });
  const result = images(first, second);
  assert.equal(result.length, 3);
  assert.equal(new Set(result.map(fact => fact.key)).size, 3);
  assert.ok(result.every(fact => fact.ref.source === asset && !('readPath' in fact)));
});

test('structured and artifact resources admit only recorded members: originals and complete bundles', () => {
  const oldPng = { type: 'asset', asset: { ...member('a'), media_type: 'image/png', url: '/api/assets/old.png' } };
  const recorded = { type: 'asset', asset: original('image/jpeg', 'jpg', '2') };
  const text = { type: 'asset', asset: { url: `/api/assets/${'3'.repeat(64)}.txt`, digest: '3'.repeat(64), media_type: 'text/plain; charset=utf-8', byte_len: 12 } };
  const result = images(tool({ type: 'structured_output', document: { type: 'inline', title: 'Chart', summary: null, blocks: [], resources: [oldPng, asset, text, recorded] } }),
    tool({ type: 'external', provider: null, operation: 'render', subject: null, fields: [], resources: [asset] }));
  assert.equal(result.length, 3);
  assert.ok(result.every(fact => isRecordedImageAsset(fact.ref.source.asset)));
  assert.deepEqual(result.map(fact => fact.ref.source.asset.media_type), ['image/webp', 'image/jpeg', 'image/webp']);
  assert.equal(new Set(result.map(fact => fact.key)).size, 3);
});

test('one transcript holds a legacy bundle beside a recorded original, in order', () => {
  const legacy = entry({ type: 'user', text: inline('[Image #1]'), attachments: [attachment()] });
  const recorded = attachment({ id: '2', media_type: 'image/png', byte_len: 4096, source: { type: 'asset', asset: original() } });
  const readRow = read('/gone.png', { attachments: [recorded, attachment({ id: '3' })] });
  const result = timelineImages([legacy, readRow]);
  assert.deepEqual(result.map(fact => [fact.ref.source.asset.media_type, isLegacyImageBundle(fact.ref.source.asset)]),
    [['image/webp', true], ['image/png', false], ['image/webp', true]]);
  assert.ok(result.every(fact => isRecordedImageAsset(fact.ref.source.asset)));
});

test('a recorded original is accepted in each of the six formats', () => {
  for (const [mediaType, extension] of FORMATS) assert.equal(isRecordedImageAsset(original(mediaType, extension)), true, mediaType);
  assert.equal(isRecordedImageAsset({ ...member('a') }), true, 'a WebP member with no rendition map is a recorded WebP');
  assert.equal(isLegacyImageBundle(original()), false);
});

test('a recorded original rejects mismatched, remote, empty and non-image members', () => {
  for (const change of [
    value => { value.url = `/api/assets/${value.digest}.jpg`; },
    value => { value.url = `/api/assets/${'2'.repeat(64)}.png`; },
    value => { value.digest = 'F'.repeat(64); value.url = `/api/assets/${value.digest}.png`; },
    value => { value.digest = '1'.repeat(63); value.url = `/api/assets/${value.digest}.png`; },
    value => { value.url = `https://example.test/api/assets/${value.digest}.png`; },
    value => { value.url = `file:///api/assets/${value.digest}.png`; },
    value => { value.url = `/api/assets/${value.digest}.png?image=full&v=1`; },
    value => { value.byte_len = 0; },
    value => { value.media_type = 'image/svg+xml'; value.url = `/api/assets/${value.digest}.svg`; },
    value => { value.media_type = 'image/avif'; value.url = `/api/assets/${value.digest}.avif`; },
    value => { value.media_type = 'image/jpg'; value.url = `/api/assets/${value.digest}.jpg`; },
    value => { value.media_type = 'image/vnd.microsoft.icon'; value.url = `/api/assets/${value.digest}.ico`; },
    value => { value.media_type = 'text/plain; charset=utf-8'; value.url = `/api/assets/${value.digest}.txt`; },
    value => { value.media_type = 'application/pdf'; value.url = `/api/assets/${value.digest}.pdf`; },
    value => { value.image_variants = {}; },
    value => { value.image_variants = { tile: member('c') }; },
  ]) { const value = original(); change(value); assert.equal(isRecordedImageAsset(value), false, JSON.stringify(value)); }
  // A malformed row without its asset is refused, as the bundle-only gate did.
  for (const missing of [undefined, null]) assert.equal(isRecordedImageAsset(missing), false, String(missing));
});

test('a legacy bundle must be complete, immutable WebP throughout', () => {
  assert.equal(isRecordedImageAsset(optimized()), true);
  assert.equal(isLegacyImageBundle(optimized()), true);
  for (const change of [
    value => { value.media_type = 'image/png'; },
    value => { delete value.image_variants.tile2x; },
    value => { delete value.image_variants.tile2x; delete value.image_variants.preview; },
    value => { value.image_variants = { rail: member('b') }; },
    value => { value.url = 'https://example.test/image.webp'; },
    value => { value.image_variants.preview.url = 'file:///tmp/image.webp'; },
    value => { value.image_variants.tile.digest = 'f'.repeat(64); },
    value => { value.image_variants.rail.byte_len = 0; },
    value => { value.image_variants.preview = original(); },
  ]) { const value = optimized(); change(value); assert.equal(isRecordedImageAsset(value), false, JSON.stringify(value)); }
});

test('byte sniffing remains available to the ordinary file viewer', () => {
  assert.equal(imageMediaTypeFromBytes(Uint8Array.from([137,80,78,71,13,10,26,10])), 'image/png');
  assert.equal(imageMediaTypeFromBytes(Uint8Array.from([255,216,255,224])), 'image/jpeg');
  assert.equal(imageMediaTypeFromBytes(Uint8Array.from([66,77,0,0])), 'image/bmp');
  assert.equal(imageMediaTypeFromBytes(Uint8Array.from([0,0,1,0])), 'image/x-icon');
  assert.equal(imageMediaTypeFromBytes(new TextEncoder().encode('not an image')), null);
});
