const test = require('node:test');
const assert = require('node:assert/strict');

const path = require('node:path');
const { argsFor, executeRosettad, parseProfile, validateAgents } = require('../src/lib/daemon');

test('profile report becomes structured metrics', () => {
  const metrics = parseProfile(`files walked                    42
walk time                      12.5 ms
read throughput               123.4 MiB/s
normalize throughput         45000 records/s
`);
  assert.deepEqual(metrics['files walked'], { value: 42, unit: 'count' });
  assert.deepEqual(metrics['walk time'], { value: 12.5, unit: 'ms' });
  assert.deepEqual(metrics['read throughput'], { value: 123.4, unit: 'MiB/s' });
  assert.deepEqual(metrics['normalize throughput'], { value: 45000, unit: 'records/s' });
});

test('daemon args are arrays and preserve explicit agent boundaries', () => {
  assert.deepEqual(
    argsFor('ingest', {
      db: '/tmp/result.sqlite',
      home: '/tmp/home with spaces',
      agents: ['codex', 'claude-code'],
      profile: true,
      json: true,
    }),
    [
      'ingest', '--db', '/tmp/result.sqlite', '--home', '/tmp/home with spaces',
      '--agent', 'codex', '--agent', 'claude-code', '--profile', '--json',
    ],
  );
});

test('agent selection rejects unknown values and duplicates collapse', () => {
  assert.deepEqual(validateAgents(['codex', 'codex']), ['codex']);
  assert.throws(() => validateAgents(['codex; rm']), /unknown agent kind/);
  assert.throws(() => validateAgents([]), /select at least one agent/);
});

test('an older daemon gets an actionable protocol error', async () => {
  const fixture = path.join(__dirname, 'fixtures', 'stale-daemon');
  const command = executeRosettad(process.execPath, [fixture]);
  await assert.rejects(command.promise, /predates the ingest-bench JSON protocol/);
});
