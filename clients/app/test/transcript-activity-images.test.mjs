import assert from 'node:assert/strict';
import { test } from 'node:test';
import { imageMediaTypeFromBytes } from '../renderer/src/lib/image-media.ts';
import { activityImages } from '../renderer/src/lib/transcript/images.ts';
import { ledgerLine } from '../renderer/src/lib/transcript/index.ts';
import { tool, read, file, inline } from './support/ledger-fixtures.mjs';

const images = (...rows) => activityImages(rows.map(row => ({ kind: 'line', line: ledgerLine(row) })));
const asset = { type: 'asset', asset: { url: '/api/assets/image.png', digest: 'a'.repeat(64), media_type: 'image/png', byte_len: 42 } };

test('activity images preserve occurrence order and exact target addresses', () => {
  const first = tool({ type: 'file_read', targets: [
    { resource: file('/demo/README.md'), range: null, media_kind: null },
    { resource: file('/demo/screen.PNG'), range: null, media_kind: null },
    { resource: file('/demo/no-extension'), range: null, media_kind: 'image' },
  ], outcome: null });
  const second = read('/demo/screen.PNG');
  const result = images(first, second);
  assert.equal(result.length, 3);
  assert.deepEqual(result.map(fact => fact.readPath.read.targetIndex), [1, 2, 0]);
  assert.deepEqual(result.map(fact => fact.readPath.read.timelineId), [first.timeline_id, first.timeline_id, second.timeline_id]);
  assert.equal(result[0].mediaType, 'image/png');
  assert.equal(new Set(result.map(fact => fact.key)).size, 3);
});

test('image artifacts include stored, local, and unavailable reference slots', () => {
  const result = images(tool({ type: 'artifact', operation: 'create', artifact_kind: 'image', name: 'Design',
    description: null, resources: [asset, file('/demo/design.png'), { type: 'url', url: 'https://example.com/design.png' }],
    media: { media_type: 'image/png', width: 1280, height: 800, duration_ms: null } }));
  assert.deepEqual(result.map(fact => fact.ref.source.type), ['asset', 'reference', 'reference']);
  assert.equal(result[1].readPath.read.targetIndex, 1);
  assert.equal(result[2].readPath, undefined);
  assert.equal(result[2].missing, true);
  assert.equal(result[0].width, 1280);
});

test('workflow images retain server indices after script and duplicate resources', () => {
  const row = tool({ type: 'workflow', name: 'Render', path: file('/demo/render.py'), arguments: null,
    objective: null, resume_id: null, background: false, task_id: null, run_id: null, attempt: null,
    callback_summary: null, callback_result: null, usage: null,
    run: { script_path: file('/demo/render.py'), resources: [file('/demo/render.py'), file('/demo/a.png'), file('/demo/a.png'), file('/demo/b.jpg')],
      status: "completed", phases: [], workers: [], logs: [], summary: null, result: null, usage: null, error: null } });
  const result = images(row);
  assert.deepEqual(result.map(fact => fact.readPath.read.targetIndex), [1, 2]);
  assert.deepEqual(result.map(fact => fact.name), ['a.png', 'b.jpg']);
});

test('structured documents and external tools expose canonical image assets only', () => {
  const result = images(tool({ type: 'structured_output', document: { type: 'inline', title: 'Chart', summary: null, blocks: [], resources: [asset] } }),
    tool({ type: 'external', provider: null, operation: 'render', subject: null, fields: [], resources: [asset] }));
  assert.equal(result.length, 2);
  assert.equal(result.every(fact => fact.ref.source.type === 'asset'), true);
  assert.notEqual(result[0].key, result[1].key);
});

test('ordinary text, directories, and command text never become image requests', () => {
  assert.deepEqual(images(tool({ type: 'file_read', targets: [{ resource: { type: 'directory', path: '/demo/images.png', resolved: true }, range: null, media_kind: null }], outcome: null }), read('/demo/readme.md'), tool({ type: 'shell', command: inline('cat image.png'), cwd: null, timeout_ms: null, background: false, outcome: null })), []);
});

test('extensionless image targets select an image viewer from file bytes', () => {
  assert.equal(imageMediaTypeFromBytes(Uint8Array.from([137,80,78,71,13,10,26,10])), 'image/png');
  assert.equal(imageMediaTypeFromBytes(Uint8Array.from([255,216,255,224])), 'image/jpeg');
  assert.equal(imageMediaTypeFromBytes(new TextEncoder().encode('GIF89a')), 'image/gif');
  assert.equal(imageMediaTypeFromBytes(new TextEncoder().encode('<svg xmlns="http://www.w3.org/2000/svg">')), 'image/svg+xml');
  assert.equal(imageMediaTypeFromBytes(new TextEncoder().encode('not an image')), null);
});
