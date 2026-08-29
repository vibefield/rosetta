'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { randomBytes } = require('node:crypto');

const { assertDirectory, assertFile, validateAgents } = require('./daemon');
const {
  concreteJsonlPath,
  profileSelection,
  runRosettad,
  safeJoin,
} = require('../../scripts/profile-cold-start');

const PREVIEW_WINDOW_BYTES = 512 * 1024;
const PREVIEW_RECORDS = 8;
const PREVIEW_TEXT_BYTES = 48 * 1024;
const MAX_PARSE_BYTES = 2 * 1024 * 1024;

function readRange(file, start, length) {
  const descriptor = fs.openSync(file, 'r');
  const buffer = Buffer.allocUnsafe(length);
  try {
    const count = fs.readSync(descriptor, buffer, 0, length, start);
    return buffer.subarray(0, count);
  } finally {
    fs.closeSync(descriptor);
  }
}

function withoutCr(buffer) {
  return buffer.at(-1) === 0x0d ? buffer.subarray(0, -1) : buffer;
}

function recordPreview(buffer, { offset, complete, location, ordinal = null }) {
  const payload = withoutCr(buffer);
  const truncated = payload.length > PREVIEW_TEXT_BYTES;
  const raw = payload.subarray(0, PREVIEW_TEXT_BYTES).toString('utf8');
  let validJson = false;
  let parsed = null;
  let parseError = null;
  let topLevel = null;
  let keys = [];
  if (complete && payload.length <= MAX_PARSE_BYTES) {
    try {
      const value = JSON.parse(payload.toString('utf8'));
      validJson = true;
      topLevel = Array.isArray(value) ? 'array' : value === null ? 'null' : typeof value;
      if (value && typeof value === 'object' && !Array.isArray(value)) keys = Object.keys(value).slice(0, 40);
      const pretty = JSON.stringify(value, null, 2);
      parsed = pretty.length > PREVIEW_TEXT_BYTES
        ? `${pretty.slice(0, PREVIEW_TEXT_BYTES)}\n… preview truncated …`
        : pretty;
    } catch (error) {
      parseError = error.message;
    }
  } else if (!complete) {
    parseError = 'incomplete record in bounded preview window';
  } else {
    parseError = `record exceeds ${MAX_PARSE_BYTES} byte preview parser bound`;
  }
  return {
    location,
    ordinal,
    offset,
    bytes: payload.length,
    complete,
    truncated,
    raw: truncated ? `${raw}\n… preview truncated …` : raw,
    validJson,
    parsed,
    parseError,
    topLevel,
    keys,
  };
}

function recordsInWindow(buffer, absoluteStart, { dropLeadingPartial, reachesEof }) {
  const records = [];
  let cursor = 0;
  if (dropLeadingPartial) {
    const firstNewline = buffer.indexOf(0x0a);
    if (firstNewline < 0) return records;
    cursor = firstNewline + 1;
  }
  while (cursor < buffer.length) {
    const newline = buffer.indexOf(0x0a, cursor);
    if (newline < 0) {
      if (reachesEof && cursor < buffer.length) {
        records.push({ buffer: buffer.subarray(cursor), offset: absoluteStart + cursor, complete: false });
      }
      break;
    }
    records.push({ buffer: buffer.subarray(cursor, newline), offset: absoluteStart + cursor, complete: true });
    cursor = newline + 1;
  }
  return records;
}

function inspectJsonl(file) {
  const resolved = assertFile(file, 'JSONL file');
  const stat = fs.statSync(resolved);
  const headLength = Math.min(stat.size, PREVIEW_WINDOW_BYTES);
  const headBuffer = readRange(resolved, 0, headLength);
  const headRecords = recordsInWindow(headBuffer, 0, {
    dropLeadingPartial: false,
    reachesEof: headLength === stat.size,
  });

  let tailRecords = [];
  let tailBytesRead = 0;
  if (stat.size > PREVIEW_WINDOW_BYTES) {
    const tailStart = Math.max(0, stat.size - PREVIEW_WINDOW_BYTES);
    const tailBuffer = readRange(resolved, tailStart, stat.size - tailStart);
    tailBytesRead = tailBuffer.length;
    tailRecords = recordsInWindow(tailBuffer, tailStart, {
      dropLeadingPartial: tailStart > 0,
      reachesEof: true,
    });
  } else {
    tailRecords = headRecords;
  }

  const first = headRecords.slice(0, PREVIEW_RECORDS).map((record, index) => recordPreview(record.buffer, {
    ...record,
    location: 'head',
    ordinal: index + 1,
  }));
  const firstOffsets = new Set(first.map((record) => record.offset));
  const last = tailRecords
    .slice(-PREVIEW_RECORDS)
    .filter((record) => !firstOffsets.has(record.offset))
    .map((record) => recordPreview(record.buffer, { ...record, location: 'tail' }));
  const finalByte = stat.size ? readRange(resolved, stat.size - 1, 1).at(0) : null;

  return {
    file: resolved,
    name: path.basename(resolved),
    bytes: stat.size,
    modifiedMs: stat.mtimeMs,
    previewBytesRead: headBuffer.length + tailBytesRead + (stat.size ? 1 : 0),
    previewWindowBytes: PREVIEW_WINDOW_BYTES,
    trailingNewline: finalByte === 0x0a,
    records: [...first, ...last],
    bounded: stat.size > PREVIEW_WINDOW_BYTES,
  };
}

async function streamSpecs(binary, agent) {
  const selectedAgent = validateAgents([agent])[0];
  const result = await runRosettad(assertFile(binary, 'rosettad binary'), [
    'streams', '--agent', selectedAgent, '--json',
  ]);
  return result.events.filter((event) => event.event === 'stream-spec' && event.driver === 'json-lines');
}

function rankJsonlSources(events, limit = 25) {
  const boundedLimit = Math.max(1, Math.min(100, Number(limit) || 25));
  return events
    .filter((event) => event.event === 'source' && event.driver === 'json-lines')
    .sort((left, right) => Number(right.bytes) - Number(left.bytes)
      || String(left.relativePath).localeCompare(String(right.relativePath)))
    .slice(0, boundedLimit)
    .map((source) => ({
      ...source,
      file: safeJoin(source.resolvedRoot, source.relativePath),
    }));
}

async function sourceInventory(binary, home, agent, limit = 25) {
  const selectedAgent = validateAgents([agent])[0];
  const result = await runRosettad(assertFile(binary, 'rosettad binary'), [
    'sources', '--home', assertDirectory(home, 'home directory'), '--agent', selectedAgent, '--json',
  ]);
  const admitted = result.events.filter((event) => event.event === 'source' && event.driver === 'json-lines');
  return {
    total: admitted.length,
    sources: rankJsonlSources(admitted, limit),
  };
}

function auditRunName(now = new Date()) {
  const timestamp = now.toISOString().replace(/[:.]/g, '-').replace('T', '_').replace('Z', '');
  return `run-jsonl-audit-${timestamp}-${randomBytes(3).toString('hex')}`;
}

class JsonlAuditRun {
  constructor(config, emit = () => {}) {
    this.emit = emit;
    this.active = null;
    this.cancelled = false;
    this.config = {
      agent: validateAgents([config.agent])[0],
      binary: assertFile(config.binary, 'rosettad binary'),
      files: [...new Set(config.files || [])].map((file) => assertFile(file, 'JSONL file')),
      outputDir: assertDirectory(config.outputDir, 'audit output directory', { create: true }),
      stream: String(config.stream || ''),
    };
    if (!this.config.files.length) throw new Error('choose at least one JSONL file');
    if (this.config.files.length > 32) throw new Error('one audit run is limited to 32 JSONL files');
    if (!this.config.stream) throw new Error('choose an adapter stream');
  }

  cancel() {
    this.cancelled = true;
    this.active?.cancel();
  }

  async execute() {
    const specs = await streamSpecs(this.config.binary, this.config.agent);
    const spec = specs.find((candidate) => candidate.stream === this.config.stream);
    if (!spec) throw new Error(`${this.config.agent} has no admitted JSON-lines stream ${this.config.stream}`);
    const workDir = path.join(this.config.outputDir, auditRunName());
    const reportPath = path.join(workDir, 'cold-stage-report.json');
    const selected = this.config.files.map((file, index) => ({
      sourcePath: file,
      relativePath: concreteJsonlPath(spec.glob, file, index),
      bytes: fs.statSync(file).size,
      stream: spec.stream,
      driver: spec.driver,
      dataRoot: spec.dataRoot,
    }));
    this.emit({ type: 'audit-start', workDir, files: this.config.files, spec });
    try {
      const result = await profileSelection({
        agent: this.config.agent,
        binary: this.config.binary,
        selected,
        workDir,
        reportPath,
        onCommand: (command) => { this.active = command; },
        onEvent: (event) => {
          if (event.event === 'stage-profile') this.emit({ type: 'audit-stage', event });
        },
        onLog: (entry) => this.emit({ type: 'log', phase: 'jsonl-audit', ...entry }),
      });
      const audit = {
        ...result,
        inputFiles: this.config.files,
        spec,
      };
      this.emit({ type: 'audit-complete', audit });
      return audit;
    } catch (error) {
      this.emit({
        type: this.cancelled ? 'audit-cancelled' : 'audit-failed',
        message: error.message,
        workDir,
      });
      throw error;
    } finally {
      this.active = null;
    }
  }
}

module.exports = {
  JsonlAuditRun,
  auditRunName,
  inspectJsonl,
  rankJsonlSources,
  sourceInventory,
  streamSpecs,
};
