const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const { auditRunName, inspectJsonl, rankJsonlSources } = require('../src/lib/jsonl-audit');

test('JSONL inspection returns bounded raw and parsed content without hiding a torn tail', () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'rosetta-jsonl-preview-'));
  const file = path.join(directory, 'input.jsonl');
  try {
    fs.writeFileSync(file, '{"type":"first","value":1}\n{"type":"torn"');
    const preview = inspectJsonl(file);
    assert.equal(preview.records.length, 2);
    assert.equal(preview.records[0].validJson, true);
    assert.equal(preview.records[0].topLevel, 'object');
    assert.deepEqual(preview.records[0].keys, ['type', 'value']);
    assert.equal(preview.records[1].complete, false);
    assert.match(preview.records[1].parseError, /incomplete/);
    assert.equal(preview.trailingNewline, false);
  } finally {
    fs.rmSync(directory, { recursive: true, force: false });
  }
});

test('audit run names are sortable and stage scoped', () => {
  const name = auditRunName(new Date('2026-08-29T12:34:56.789Z'));
  assert.match(name, /^run-jsonl-audit-2026-08-29_12-34-56-789-[a-f0-9]{6}$/);
});

test('source ranking keeps only the largest adapter-admitted JSON-lines files', () => {
  const ranked = rankJsonlSources([
    { event: 'source', driver: 'inventory', bytes: 900, resolvedRoot: '/tmp/root', relativePath: 'ignored.jsonl' },
    { event: 'source', driver: 'json-lines', stream: 'main', bytes: 10, resolvedRoot: '/tmp/root', relativePath: 'small.jsonl' },
    { event: 'source', driver: 'json-lines', stream: 'subagent', bytes: 400, resolvedRoot: '/tmp/root', relativePath: 'large.jsonl' },
    { event: 'other', driver: 'json-lines', bytes: 800, resolvedRoot: '/tmp/root', relativePath: 'not-a-source.jsonl' },
    { event: 'source', driver: 'json-lines', stream: 'main', bytes: 200, resolvedRoot: '/tmp/root', relativePath: 'middle.jsonl' },
  ], 2);

  assert.deepEqual(ranked.map((source) => source.relativePath), ['large.jsonl', 'middle.jsonl']);
  assert.deepEqual(ranked.map((source) => source.file), ['/tmp/root/large.jsonl', '/tmp/root/middle.jsonl']);
});
