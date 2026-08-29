#!/usr/bin/env node
'use strict';

const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { createHash } = require('node:crypto');
const { spawnSync } = require('node:child_process');

const {
  assertDirectory,
  assertFile,
  executeRosettad,
  parseProfile,
  validateAgents,
} = require('../src/lib/daemon');

const REPO_ROOT = path.resolve(__dirname, '../../..');
const FORMAT = 'rosetta-cold-stage-profile/1';

function usage() {
  return `Usage: node tools/ingest-bench/scripts/profile-cold-start.js [options]

Profiles a real cold ingest over an immutable snapshot of the N largest
adapter-admitted sources. Source selection and snapshot preparation are timed
separately and excluded from the cold-ingest wall clock.

Options:
  --binary PATH     rosettad built from this checkout (auto-detected by default)
  --home DIR        source home to inspect (default: current home)
  --agent KIND      adapter to profile (default: claude-code)
  --count N         number of largest JSON-lines sources (default: 3)
  --report PATH     write a sanitized JSON report; refuses to overwrite
  --work-dir DIR    keep private snapshot/DB/ledger/search artifacts here;
                    DIR must not already exist (default: secure temporary dir)
  --help            show this help
`;
}

function targetDirectory() {
  const metadata = spawnSync('cargo', ['metadata', '--format-version', '1', '--no-deps'], {
    cwd: REPO_ROOT,
    encoding: 'utf8',
    timeout: 10_000,
  });
  if (metadata.status !== 0) return path.join(REPO_ROOT, 'target');
  try {
    return JSON.parse(metadata.stdout).target_directory;
  } catch {
    return path.join(REPO_ROOT, 'target');
  }
}

function defaultBinary() {
  const executable = process.platform === 'win32' ? 'rosettad.exe' : 'rosettad';
  const target = targetDirectory();
  const candidates = [path.join(target, 'release', executable), path.join(target, 'debug', executable)];
  return candidates.find((candidate) => fs.existsSync(candidate)) || candidates[0];
}

function takeValue(argv, index) {
  if (index + 1 >= argv.length) throw new Error(`${argv[index]} needs a value`);
  return argv[index + 1];
}

function parseArgs(argv) {
  const options = {
    agent: 'claude-code',
    binary: defaultBinary(),
    count: 3,
    home: os.homedir(),
    report: null,
    workDir: null,
  };
  for (let at = 0; at < argv.length;) {
    const flag = argv[at];
    if (flag === '--help') return { ...options, help: true };
    const value = takeValue(argv, at);
    if (flag === '--binary') options.binary = value;
    else if (flag === '--home') options.home = value;
    else if (flag === '--agent') options.agent = value;
    else if (flag === '--count') {
      options.count = Number(value);
      if (!Number.isSafeInteger(options.count) || options.count < 1) {
        throw new Error(`--count needs a positive integer, got ${value}`);
      }
    } else if (flag === '--report') options.report = path.resolve(value);
    else if (flag === '--work-dir') options.workDir = path.resolve(value);
    else throw new Error(`unknown option ${flag}`);
    at += 2;
  }
  options.agent = validateAgents([options.agent])[0];
  options.binary = assertFile(options.binary, 'rosettad binary');
  options.home = assertDirectory(options.home, 'home directory');
  return options;
}

function safeJoin(root, portableRelative) {
  const resolvedRoot = path.resolve(root);
  const target = path.resolve(resolvedRoot, ...portableRelative.split('/'));
  if (target !== resolvedRoot && !target.startsWith(`${resolvedRoot}${path.sep}`)) {
    throw new Error(`source path escaped its runtime root: ${portableRelative}`);
  }
  return target;
}

function safeToken(value, fallback) {
  const token = value.replace(/[^a-zA-Z0-9._-]+/g, '-').replace(/^-+|-+$/g, '');
  return token || fallback;
}

/// Materialize one concrete path from the canonical layout's small glob
/// language. The staged path is verified through `rosettad sources` before
/// ingest, so this helper never becomes an independent admission authority.
function concreteJsonlPath(glob, sourcePath, index = 0) {
  const original = path.basename(sourcePath);
  const originalStem = path.basename(original, path.extname(original));
  let wildcard = false;
  const segments = glob.split('/').map((segment, segmentIndex, all) => {
    if (segment.includes('<') || segment.includes('>')) {
      throw new Error(`cannot materialize an external fixture glob: ${glob}`);
    }
    if (!segment.includes('*')) return segment;
    wildcard = true;
    if (segment === '**') return `audit-${index + 1}`;
    const last = segmentIndex === all.length - 1;
    let replacement;
    if (last && segment === '*') replacement = safeToken(original, `input-${index + 1}.jsonl`);
    else if (last) replacement = `${safeToken(originalStem, 'input')}-${index + 1}`;
    else replacement = `audit-${index + 1}-${segmentIndex + 1}`;
    return segment.replace(/\*/g, replacement);
  });
  if (!wildcard && index > 0) {
    throw new Error(`stream glob ${glob} represents only one source file`);
  }
  const relativePath = segments.join('/');
  if (relativePath.includes('*') || !relativePath.endsWith('.jsonl')) {
    throw new Error(`stream glob did not materialize a JSONL path: ${glob}`);
  }
  return relativePath;
}

function expandDataRoot(dataRoot, home) {
  if (dataRoot === '~') return home;
  if (dataRoot.startsWith('~/')) return path.join(home, dataRoot.slice(2));
  throw new Error(`the isolated profiler requires a home-relative data root, got ${dataRoot}`);
}

function stableStat(file) {
  const stat = fs.statSync(file, { bigint: true });
  if (!stat.isFile()) throw new Error(`selected source is not a regular file: ${file}`);
  return {
    dev: stat.dev,
    ino: stat.ino,
    size: stat.size,
    mtimeNs: stat.mtimeNs,
  };
}

function sameStat(left, right) {
  return left.dev === right.dev
    && left.ino === right.ino
    && left.size === right.size
    && left.mtimeNs === right.mtimeNs;
}

function sha256File(file) {
  const hash = createHash('sha256');
  const descriptor = fs.openSync(file, 'r');
  const buffer = Buffer.allocUnsafe(1024 * 1024);
  try {
    let offset = 0;
    while (true) {
      const count = fs.readSync(descriptor, buffer, 0, buffer.length, offset);
      if (count === 0) break;
      hash.update(buffer.subarray(0, count));
      offset += count;
    }
    return hash.digest('hex');
  } finally {
    fs.closeSync(descriptor);
  }
}

function copyStableAndVerify(source, destination, expectedBytes) {
  const before = stableStat(source);
  if (Number(before.size) !== expectedBytes) {
    throw new Error(`source size changed since inventory: ${source}`);
  }
  fs.mkdirSync(path.dirname(destination), { recursive: true, mode: 0o700 });
  const input = fs.openSync(source, 'r');
  const output = fs.openSync(destination, 'wx', 0o600);
  const hash = createHash('sha256');
  const buffer = Buffer.allocUnsafe(1024 * 1024);
  try {
    let readOffset = 0;
    while (true) {
      const count = fs.readSync(input, buffer, 0, buffer.length, readOffset);
      if (count === 0) break;
      hash.update(buffer.subarray(0, count));
      let written = 0;
      while (written < count) {
        written += fs.writeSync(output, buffer, written, count - written, readOffset + written);
      }
      readOffset += count;
    }
    fs.fsyncSync(output);
  } finally {
    fs.closeSync(input);
    fs.closeSync(output);
  }
  const after = stableStat(source);
  if (!sameStat(before, after)) throw new Error(`source changed while it was frozen: ${source}`);
  const sourceDigest = hash.digest('hex');
  const copyDigest = sha256File(destination);
  if (sourceDigest !== copyDigest) throw new Error(`snapshot checksum mismatch: ${source}`);
  const sourceModified = new Date(Number(before.mtimeNs / 1_000_000n));
  fs.utimesSync(destination, sourceModified, sourceModified);
  fs.chmodSync(destination, 0o444);
}

function directoriesBelow(root) {
  const directories = [];
  function visit(directory) {
    directories.push(directory);
    for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
      if (entry.isDirectory()) visit(path.join(directory, entry.name));
    }
  }
  visit(root);
  return directories;
}

function freezeDirectories(root) {
  for (const directory of directoriesBelow(root).reverse()) fs.chmodSync(directory, 0o555);
}

function thawDirectories(root) {
  if (!fs.existsSync(root)) return;
  for (const directory of directoriesBelow(root)) fs.chmodSync(directory, 0o700);
}

function treeBytes(root) {
  if (!fs.existsSync(root)) return 0;
  const stat = fs.statSync(root);
  if (stat.isFile()) return stat.size;
  let total = 0;
  for (const entry of fs.readdirSync(root)) total += treeBytes(path.join(root, entry));
  return total;
}

function runRosettad(binary, args, {
  onEvent = () => {},
  onLog = () => {},
  onCommand = () => {},
} = {}) {
  const events = [];
  const command = executeRosettad(binary, args, {
    onEvent(event) {
      events.push(event);
      onEvent(event);
    },
    onLog,
  });
  onCommand(command);
  return command.promise
    .then((process) => ({ events, process }))
    .finally(() => onCommand(null));
}

function metricMap(event) {
  return event ? parseProfile(event.report) : {};
}

function metricMs(metrics, name) {
  const metric = metrics[name];
  return metric?.unit === 'ms' ? metric.value : null;
}

function countMetric(metrics, name) {
  const metric = metrics[name];
  return metric?.unit === 'count' ? metric.value : null;
}

function stageEvent(events, name, agent = null) {
  return events.find((event) => event.event === 'stage-profile'
    && event.stage === name
    && (agent === null || event.agent === agent));
}

function round(value, digits = 3) {
  return value === null || value === undefined ? null : Number(value.toFixed(digits));
}

function buildStageReport(events, processWallMs, agent) {
  const phases = Object.fromEntries(
    ['discovery', 'metadata', 'stream', 'ledger-durability', 'finalize-publish', 'search']
      .map((name) => {
        const event = stageEvent(events, name, ['metadata', 'stream'].includes(name) ? agent : null);
        return [name, {
          wallMs: event ? event.wallMs : null,
          metrics: metricMap(event),
        }];
      }),
  );
  const stream = phases.stream.metrics;
  const finalize = phases['finalize-publish'].metrics;
  const ledgerAppendMs = metricMs(stream, 'ledger write time');
  const readMs = metricMs(stream, 'read time');
  const normalizeMs = metricMs(stream, 'normalize time');
  const applyMs = metricMs(stream, 'apply time');
  const streamKnown = [readMs, normalizeMs, ledgerAppendMs, applyMs]
    .filter((value) => value !== null)
    .reduce((sum, value) => sum + value, 0);
  const phaseWallTotal = Object.values(phases)
    .map((phase) => phase.wallMs)
    .filter((value) => value !== null)
    .reduce((sum, value) => sum + value, 0);

  return {
    processWallMs: round(processWallMs),
    phaseWallTotalMs: round(phaseWallTotal),
    processResidualMs: round(Math.max(0, processWallMs - phaseWallTotal)),
    stages: [
      {
        step: 1,
        name: 'discovery',
        measuredMs: round(phases.discovery.wallMs),
        basis: 'phase wall',
        details: { walkMs: round(metricMs(phases.discovery.metrics, 'walk time')) },
      },
      {
        step: 2,
        name: 'bounded metadata skim',
        measuredMs: round(phases.metadata.wallMs),
        basis: 'metadata phase wall (includes its filtered walk and metadata writes)',
        details: {
          walkMs: round(metricMs(phases.metadata.metrics, 'walk time')),
          skimMs: round(metricMs(phases.metadata.metrics, 'skim time')),
          ledgerMs: round(metricMs(phases.metadata.metrics, 'ledger write time')),
          sqliteApplyMs: round(metricMs(phases.metadata.metrics, 'apply time')),
        },
      },
      {
        step: 3,
        name: 'stream and frame full source records',
        measuredMs: round(readMs),
        basis: 'tail read/framing timer; stream phase wall is reported in details',
        details: {
          streamPhaseWallMs: round(phases.stream.wallMs),
          bytesRead: countMetric(stream, 'bytes read'),
          recordsFramed: countMetric(stream, 'records framed'),
          rawLedgerAppendMs: round(ledgerAppendMs),
          streamResidualMs: phases.stream.wallMs === null
            ? null
            : round(Math.max(0, phases.stream.wallMs - streamKnown)),
        },
      },
      {
        step: 4,
        name: 'parse',
        measuredMs: null,
        basis: 'not separately observable: adapter ingest_line combines parse and normalize',
        details: {},
      },
      {
        step: 5,
        name: 'normalize (including parse)',
        measuredMs: round(normalizeMs),
        basis: 'combined adapter ingest_line timer',
        details: {
          linesParsed: countMetric(stream, 'lines parsed'),
          linesUnknown: countMetric(stream, 'lines unknown-type'),
          linesHardError: countMetric(stream, 'lines hard-error'),
          opsEmitted: countMetric(stream, 'ops emitted'),
        },
      },
      {
        step: 6,
        name: 'fold / merge',
        measuredMs: null,
        basis: 'not a standalone boundary: enrichment is unmetered and merge work is inside canonical apply',
        details: {},
      },
      {
        step: 7,
        name: 'canonical SQLite apply',
        measuredMs: round(applyMs),
        basis: 'end-to-end Store::apply timers; child timers overlap this parent',
        details: {
          sourceStageMs: round(metricMs(stream, 'source staging time')),
          canonicalApplyMs: round(metricMs(stream, 'canonical apply time')),
          sqliteCommitMs: round(metricMs(stream, 'sqlite commit time')),
          batches: countMetric(stream, 'batches applied'),
        },
      },
      {
        step: 8,
        name: 'global finalization',
        measuredMs: round(metricMs(finalize, 'apply time')),
        basis: 'finish_bulk_load end-to-end timer',
        details: {
          renumberMs: round(metricMs(finalize, 'bulk renumber time')),
          usageBucketsMs: round(metricMs(finalize, 'bulk bucket time')),
          indexesAndAnalyzeMs: round(metricMs(finalize, 'bulk index time')),
          validationMs: round(metricMs(finalize, 'bulk validate time')),
          pragmaMs: round(metricMs(finalize, 'bulk pragma time')),
          sqliteCommitMs: round(metricMs(finalize, 'sqlite commit time')),
          finalizeAndPublishWallMs: round(phases['finalize-publish'].wallMs),
        },
      },
      {
        step: 9,
        name: 'raw-ledger durability barrier',
        measuredMs: round(phases['ledger-durability'].wallMs),
        basis: 'phase wall around deferred ledger flush/fsync',
        details: {
          ledgerWriteMs: round(metricMs(phases['ledger-durability'].metrics, 'ledger write time')),
          flushMs: round(metricMs(phases['ledger-durability'].metrics, 'ledger flush time')),
          syncMs: round(metricMs(phases['ledger-durability'].metrics, 'ledger sync time')),
          directorySyncMs: round(metricMs(phases['ledger-durability'].metrics, 'ledger directory sync time')),
        },
      },
      {
        step: 10,
        name: 'atomic publish',
        measuredMs: round(metricMs(finalize, 'publication time')),
        basis: 'publication timer inside finalize-publish phase',
        details: {},
      },
      {
        step: 11,
        name: 'detached FTS5 search build',
        measuredMs: round(phases.search.wallMs),
        basis: 'search worker synchronization phase wall',
        details: {
          searchSyncMs: round(metricMs(phases.search.metrics, 'detached search sync time')),
        },
      },
    ],
  };
}

function formatBytes(bytes) {
  const units = ['B', 'KiB', 'MiB', 'GiB'];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(unit === 0 ? 0 : 2)} ${units[unit]}`;
}

function formatMs(value) {
  return value === null ? 'combined' : `${value.toFixed(1)} ms`;
}

function printStageTable(stageReport) {
  console.log('\nCold-ingest stage costs');
  console.log('step  boundary                                  measured cost');
  for (const stage of stageReport.stages) {
    console.log(`${String(stage.step).padStart(2)}    ${stage.name.padEnd(40)} ${formatMs(stage.measuredMs).padStart(13)}`);
  }
  console.log(`\nprocess wall                               ${formatMs(stageReport.processWallMs)}`);
  console.log(`phase-wall residual (startup/setup/events) ${formatMs(stageReport.processResidualMs)}`);
  console.log('\nRows 4 and 6 are deliberately not assigned synthetic costs; see the report basis fields.');
}

async function profileSelection({
  agent,
  binary,
  selected,
  workDir: requestedWorkDir = null,
  reportPath = null,
  inventoryWallMs = null,
  preparationStarted = process.hrtime.bigint(),
  requestedLargest = null,
  admittedJsonLinesAvailable = null,
  onEvent = () => {},
  onLog = () => {},
  onCommand = () => {},
}) {
  if (!selected.length) throw new Error('select at least one JSON-lines source');
  const dataRoots = new Set(selected.map((source) => source.dataRoot));
  if (dataRoots.size !== 1) throw new Error('selected sources must share one adapter data root');

  let temporary = false;
  let workDir;
  if (requestedWorkDir) {
    workDir = path.resolve(requestedWorkDir);
    if (fs.existsSync(workDir)) throw new Error(`work directory already exists: ${workDir}`);
    fs.mkdirSync(workDir, { recursive: false, mode: 0o700 });
  } else {
    workDir = fs.mkdtempSync(path.join(os.tmpdir(), 'rosetta-cold-stage-'));
    temporary = true;
  }

  const snapshotHome = path.join(workDir, 'snapshot-home');
  const runDir = path.join(workDir, 'run');
  const db = path.join(runDir, 'rosetta.sqlite');
  const ledger = path.join(runDir, 'rosetta.ledger');
  const search = `${db}.search`;
  const snapshotRoot = expandDataRoot(selected[0].dataRoot, snapshotHome);
  fs.mkdirSync(snapshotRoot, { recursive: true, mode: 0o700 });
  fs.mkdirSync(runDir, { recursive: false, mode: 0o700 });

  try {
    const snapshotStarted = process.hrtime.bigint();
    for (const source of selected) {
      const copy = safeJoin(snapshotRoot, source.relativePath);
      copyStableAndVerify(source.sourcePath, copy, source.bytes);
    }
    freezeDirectories(snapshotHome);
    const snapshotWallMs = Number(process.hrtime.bigint() - snapshotStarted) / 1e6;

    // Verify that the synthetic snapshot still crosses the production
    // adapter admission boundary before charging anything to cold ingest.
    const admission = await runRosettad(binary, [
      'sources', '--home', snapshotHome, '--agent', agent, '--json',
    ], { onLog, onCommand });
    const admitted = new Map(admission.events
      .filter((event) => event.event === 'source')
      .map((event) => [event.relativePath, event]));
    for (const source of selected) {
      const event = admitted.get(source.relativePath);
      if (!event || event.stream !== source.stream || event.driver !== 'json-lines') {
        throw new Error(`staged source was not admitted as ${source.stream}: ${source.relativePath}`);
      }
    }
    const preparationWallMs = Number(process.hrtime.bigint() - preparationStarted) / 1e6;

    const ingest = await runRosettad(binary, [
      'ingest', '--db', db, '--ledger', ledger,
      '--home', snapshotHome, '--agent', agent, '--profile', '--json',
    ], { onEvent, onLog, onCommand });
    const stageReport = buildStageReport(ingest.events, ingest.process.durationMs, agent);
    const streamEvent = ingest.events.find((event) => event.event === 'stream' && event.agent === agent);
    const overallProfileEvent = ingest.events.findLast((event) => event.event === 'profile');
    if (!streamEvent || !overallProfileEvent) {
      throw new Error('ingest completed without required stream/profile evidence');
    }

    const totalSourceBytes = selected.reduce((sum, source) => sum + source.bytes, 0);
    const report = {
      format: FORMAT,
      capturedAt: new Date().toISOString(),
      selection: {
        agent,
        requestedLargest,
        admittedJsonLinesAvailable,
        totalSourceBytes,
        sourceFiles: selected.map((source, index) => ({
          rank: index + 1,
          bytes: source.bytes,
          stream: source.stream,
          driver: source.driver,
          stableDuringCopy: true,
          checksumVerified: true,
        })),
        privatePathsRecorded: false,
        checksumsRecorded: false,
        snapshotReadOnlyDuringRun: true,
      },
      preparation: {
        excludedFromColdWall: true,
        inventoryWallMs: round(inventoryWallMs),
        snapshotCopyHashAndVerifyWallMs: round(snapshotWallMs),
        totalWallMs: round(preparationWallMs),
        pageCacheNote: 'projection-cold, not filesystem-cache-cold; snapshot verification reads every source byte before ingest',
      },
      coldIngest: stageReport,
      outcome: {
        records: streamEvent.records,
        parsed: streamEvent.parsed,
        unknown: streamEvent.unknown,
        hardErrors: streamEvent.hardErrors,
        messagesInserted: streamEvent.messagesInserted,
        messagesUpdated: streamEvent.messagesUpdated,
        toolResultsMerged: streamEvent.toolResultsMerged,
        sessionsIndexed: streamEvent.sessionsIndexed,
        sqliteBytes: treeBytes(db),
        rawLedgerBytes: treeBytes(ledger),
        searchSidecarBytes: treeBytes(search),
        prePublicationValidationPassed: true,
      },
      implementationFacts: [
        'The current stream path parses and normalizes before appending the corresponding raw-ledger frame; ledger append still precedes SQLite apply.',
        'Parse and normalize share AgentAdapter::ingest_line and cannot currently be timed separately.',
        'There is no standalone generic fold/merge boundary; enrichment is unmetered and merge work lands inside canonical SQLite apply.',
        'The deferred raw-ledger durability barrier currently runs before global SQLite finalization.',
        'Parent timers and their child timers overlap and must not be summed.',
      ],
    };

    if (reportPath) {
      const resolvedReport = path.resolve(reportPath);
      fs.mkdirSync(path.dirname(resolvedReport), { recursive: true });
      fs.writeFileSync(resolvedReport, `${JSON.stringify(report, null, 2)}\n`, { flag: 'wx', mode: 0o600 });
    }
    return {
      report,
      paths: { workDir, snapshotHome, db, ledger, search, report: reportPath && path.resolve(reportPath) },
    };
  } finally {
    if (temporary) {
      thawDirectories(snapshotHome);
      fs.rmSync(workDir, { recursive: true, force: false });
    } else {
      // Retained evidence was permission-read-only for the measured run. Make
      // its directories traversable/removable afterward while leaving source
      // files themselves read-only.
      thawDirectories(snapshotHome);
    }
  }
}

async function main(argv) {
  const options = parseArgs(argv);
  if (options.help) {
    process.stdout.write(usage());
    return;
  }

  const preparationStarted = process.hrtime.bigint();
  const inventoryStarted = process.hrtime.bigint();
  const inventory = await runRosettad(options.binary, [
    'sources', '--home', options.home, '--agent', options.agent, '--json',
  ]);
  const inventoryWallMs = Number(process.hrtime.bigint() - inventoryStarted) / 1e6;
  const sources = inventory.events
    .filter((event) => event.event === 'source' && event.driver === 'json-lines')
    .sort((left, right) => right.bytes - left.bytes || left.relativePath.localeCompare(right.relativePath));
  if (sources.length < options.count) {
    throw new Error(`${options.agent} exposed only ${sources.length} admitted JSON-lines sources; need ${options.count}`);
  }
  const selected = sources.slice(0, options.count);
  const resolvedRoots = new Set(selected.map((source) => source.resolvedRoot));
  if (resolvedRoots.size !== 1) throw new Error('selected sources did not share one adapter data root');

  console.log(`Selected ${options.count} largest ${options.agent} JSON-lines sources:`);
  selected.forEach((source, index) => {
    const absolute = safeJoin(source.resolvedRoot, source.relativePath);
    console.log(`${index + 1}. ${formatBytes(source.bytes).padStart(10)}  ${absolute}`);
  });

  const normalized = selected.map((source) => ({
    ...source,
    sourcePath: safeJoin(source.resolvedRoot, source.relativePath),
  }));
  console.log(`\nFrozen snapshot input: ${formatBytes(selected.reduce((sum, source) => sum + source.bytes, 0))}; starting cold ingest...`);
  const result = await profileSelection({
    agent: options.agent,
    binary: options.binary,
    selected: normalized,
    workDir: options.workDir,
    reportPath: options.report,
    inventoryWallMs,
    preparationStarted,
    requestedLargest: options.count,
    admittedJsonLinesAvailable: sources.length,
    onEvent(event) {
      if (event.event === 'stage-profile') {
        console.log(`  ${event.stage.padEnd(20)} ${event.wallMs.toFixed(1)} ms`);
      }
    },
  });
  printStageTable(result.report.coldIngest);
  console.log(`\nraw source    ${formatBytes(result.report.selection.totalSourceBytes)}`);
  console.log(`raw ledger    ${formatBytes(result.report.outcome.rawLedgerBytes)}`);
  console.log(`SQLite        ${formatBytes(result.report.outcome.sqliteBytes)}`);
  console.log(`FTS5 sidecar  ${formatBytes(result.report.outcome.searchSidecarBytes)}`);
  if (options.report) console.log(`\nSanitized report: ${options.report}`);
  if (options.workDir) console.log(`Private artifacts retained at: ${result.paths.workDir}`);
}

if (require.main === module) {
  main(process.argv.slice(2)).catch((error) => {
    console.error(`cold-stage-profile: ${error.message}`);
    process.exitCode = 1;
  });
}

module.exports = {
  FORMAT,
  buildStageReport,
  concreteJsonlPath,
  parseArgs,
  profileSelection,
  runRosettad,
  safeJoin,
};
