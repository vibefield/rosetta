const test = require('node:test');
const assert = require('node:assert/strict');

const {
  buildStageReport,
  concreteJsonlPath,
  safeJoin,
} = require('../scripts/profile-cold-start');

function stage(stageName, wallMs, report, agent = null) {
  return {
    event: 'stage-profile',
    stage: stageName,
    agent,
    wallMs,
    report,
  };
}

test('cold-stage report keeps parent/child overlaps explicit', () => {
  const events = [
    stage('discovery', 10, 'walk time 8.0 ms\n'),
    stage('metadata', 20, 'walk time 2.0 ms\nskim time 3.0 ms\napply time 5.0 ms\nledger write time 1.0 ms\n', 'claude-code'),
    stage('stream', 100, `bytes read 4096
records framed 8
lines parsed 8
lines unknown-type 0
lines hard-error 0
ops emitted 12
batches applied 2
read time 10.0 ms
normalize time 20.0 ms
ledger write time 5.0 ms
apply time 40.0 ms
source staging time 3.0 ms
canonical apply time 30.0 ms
sqlite commit time 4.0 ms
`, 'claude-code'),
    stage('ledger-durability', 7, 'ledger write time 6.0 ms\nledger sync time 5.0 ms\n'),
    stage('finalize-publish', 30, `apply time 20.0 ms
bulk renumber time 2.0 ms
bulk bucket time 3.0 ms
bulk index time 4.0 ms
bulk validate time 5.0 ms
bulk pragma time 1.0 ms
sqlite commit time 2.0 ms
publication time 8.0 ms
`),
    stage('search', 25, 'detached search sync time 24.0 ms\n'),
  ];

  const report = buildStageReport(events, 200, 'claude-code');
  assert.equal(report.stages[2].measuredMs, 10);
  assert.equal(report.stages[2].details.rawLedgerAppendMs, 5);
  assert.equal(report.stages[2].details.streamResidualMs, 25);
  assert.equal(report.stages[3].measuredMs, null);
  assert.equal(report.stages[4].measuredMs, 20);
  assert.equal(report.stages[5].measuredMs, null);
  assert.equal(report.stages[6].measuredMs, 40);
  assert.equal(report.stages[6].details.canonicalApplyMs, 30);
  assert.equal(report.stages[7].measuredMs, 20);
  assert.equal(report.stages[9].measuredMs, 8);
  assert.equal(report.stages[10].measuredMs, 25);
  assert.equal(report.processResidualMs, 8);
  for (const stage of report.stages) {
    assert.ok(stage.purpose.length > 40, `step ${stage.step} needs a useful purpose`);
    assert.ok(stage.actualWork.length > 60, `step ${stage.step} needs concrete implementation text`);
  }
  assert.match(report.stages[3].actualWork, /share one call and therefore one timer/);
  assert.match(report.stages[8].actualWork, /before global SQLite finalization/);
});

test('safeJoin rejects traversal outside a runtime root', () => {
  assert.equal(safeJoin('/tmp/root', 'projects/a.jsonl'), '/tmp/root/projects/a.jsonl');
  assert.throws(() => safeJoin('/tmp/root', '../secret.jsonl'), /escaped/);
});

test('canonical stream globs materialize unique auditable JSONL paths', () => {
  assert.equal(
    concreteJsonlPath('projects/*/*.jsonl', '/tmp/my-session.jsonl', 0),
    'projects/audit-1-2/my-session-1.jsonl',
  );
  assert.equal(
    concreteJsonlPath('sessions/*/*/*/rollout-*.jsonl', '/tmp/input.jsonl', 1),
    'sessions/audit-2-2/audit-2-3/audit-2-4/rollout-input-2.jsonl',
  );
  assert.throws(
    () => concreteJsonlPath('logs/unified.jsonl', '/tmp/second.jsonl', 1),
    /only one source/,
  );
});
