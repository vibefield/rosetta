const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const {
  auditFailureReport,
  auditRunName,
  inspectJsonl,
  rankJsonlSources,
} = require('../src/lib/jsonl-audit');

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

test('failed audits retain process evidence without recording private input paths', () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'rosetta-jsonl-failure-'));
  const file = path.join(directory, 'private-session-name.jsonl');
  try {
    fs.writeFileSync(file, '{"type":"assistant"}\n');
    const error = new Error('UNIQUE constraint failed: message response block');
    error.details = {
      code: 1,
      signal: null,
      durationMs: 12.5,
      stdoutTail: [
        '{"event":"stage-profile","stage":"metadata","wallMs":3.5}',
        JSON.stringify({ event: 'boundary-evidence', private: file, content: 'do not retain me' }),
        '',
      ].join('\n'),
      stderrTail: 'rosettad: store error\n',
    };
    const report = auditFailureReport({
      agent: 'claude-code',
      stream: 'transcript',
      files: [file],
    }, error);

    assert.equal(report.outcome, 'failed');
    assert.equal(report.selection.fileCount, 1);
    assert.equal(report.selection.totalSourceBytes, fs.statSync(file).size);
    assert.equal(report.selection.privatePathsRecorded, false);
    assert.equal(report.process.code, 1);
    assert.deepEqual(report.process.stdoutTrace, [
      { event: 'stage-profile', stage: 'metadata', wallMs: 3.5 },
      { event: 'boundary-evidence' },
    ]);
    assert.match(report.process.stderrTail, /store error/);
    assert.doesNotMatch(JSON.stringify(report), /private-session-name/);
    assert.doesNotMatch(JSON.stringify(report), /do not retain me/);
  } finally {
    fs.rmSync(directory, { recursive: true, force: false });
  }
});

test('failure evidence keeps captured sizes when a selected source vanishes', () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'rosetta-jsonl-vanished-'));
  const file = path.join(directory, 'gone.jsonl');
  try {
    fs.writeFileSync(file, '{"type":"assistant"}\n');
    const bytes = fs.statSync(file).size;
    fs.unlinkSync(file);
    const report = auditFailureReport({
      agent: 'claude-code',
      stream: 'transcript',
      files: [file],
      sourceBytes: [bytes],
    }, new Error(`source vanished: ${file}`));

    assert.equal(report.selection.totalSourceBytes, bytes);
    assert.equal(report.selection.sourceSizesKnown, 1);
    assert.equal(report.error.message, 'source vanished: <source>');
  } finally {
    fs.rmSync(directory, { recursive: true, force: false });
  }
});
