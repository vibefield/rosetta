const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');

const { BenchmarkRun, FORMAT, runName, validateStage } = require('../src/lib/benchmark');

function fixture(t) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'rosetta-ingest-bench-stage-'));
  const home = path.join(directory, 'home');
  const outputDir = path.join(directory, 'output');
  const binary = path.join(directory, 'fake-rosettad');
  fs.mkdirSync(home);
  fs.mkdirSync(outputDir);
  fs.writeFileSync(binary, `#!/usr/bin/env node
const fs = require('node:fs');
const args = process.argv.slice(2);
const command = args[0];
const value = (flag) => args[args.indexOf(flag) + 1];
if (command === 'discover') {
  console.log(JSON.stringify({ event: 'discovery', agent: 'codex', installed: true }));
} else if (command === 'meta') {
  fs.writeFileSync(value('--db'), 'meta');
  console.log(JSON.stringify({ event: 'meta', agent: 'codex' }));
} else if (command === 'ingest') {
  fs.writeFileSync(value('--db'), 'projection');
  console.log(JSON.stringify({ event: 'stream', agent: 'codex' }));
}
console.log(JSON.stringify({ event: 'profile', report: 'walk time 1.5 ms' }));
`);
  fs.chmodSync(binary, 0o755);
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  return { binary, home, outputDir };
}

test('each stage runs alone and writes a stage-scoped report', async (t) => {
  const config = fixture(t);
  for (const stage of ['discovery', 'meta', 'ingest']) {
    const report = await new BenchmarkRun({ ...config, agents: ['codex'], stage }).execute();
    assert.equal(report.format, FORMAT);
    assert.equal(report.stage, stage);
    assert.equal(report.status, 'complete');
    assert.deepEqual(Object.keys(report.phases), [stage]);
    assert.equal(report.phases[stage].profile['walk time'].value, 1.5);
    assert.ok(fs.existsSync(report.paths.report));
    assert.match(path.basename(report.paths.runDir), new RegExp(`^run-${stage}-`));
    assert.equal(fs.existsSync(report.paths.metaDb), stage === 'meta');
    assert.equal(fs.existsSync(report.paths.projectionDb), stage === 'ingest');
  }
});

test('stage names are closed and appear in run directory names', () => {
  assert.equal(validateStage('meta'), 'meta');
  assert.throws(() => validateStage('all'), /unknown benchmark stage/);
  assert.match(runName('discovery', new Date('2026-08-28T12:00:00Z')), /^run-discovery-2026-08-28_12-00-00-/);
});
