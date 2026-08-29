const fs = require('node:fs');
const path = require('node:path');
const { randomBytes } = require('node:crypto');

const {
  assertDirectory,
  assertFile,
  executeRosettad,
  profileFromEvents,
  validateAgents,
} = require('./daemon');

const FORMAT = 'rosetta-ingest-bench/2';
const STAGES = new Set(['discovery', 'meta', 'ingest']);

function validateStage(stage) {
  if (!STAGES.has(stage)) throw new Error(`unknown benchmark stage: ${stage}`);
  return stage;
}

function runName(stage, now = new Date()) {
  return `run-${validateStage(stage)}-${now.toISOString().replace(/[:.]/g, '-').replace('T', '_').replace('Z', '')}-${randomBytes(3).toString('hex')}`;
}

function serializeError(error) {
  return {
    name: error?.name || 'Error',
    message: error?.message || String(error),
    details: error?.details || null,
  };
}

class BenchmarkRun {
  constructor(config, emit = () => {}) {
    this.emit = emit;
    this.active = null;
    this.cancelled = false;
    this.config = {
      binary: assertFile(config.binary, 'rosettad binary'),
      home: assertDirectory(config.home, 'home directory'),
      outputDir: assertDirectory(config.outputDir, 'output directory', { create: true }),
      agents: validateAgents(config.agents),
      stage: validateStage(config.stage),
    };
  }

  cancel() {
    this.cancelled = true;
    this.active?.cancel();
  }

  async execute() {
    const startedAt = new Date();
    const runDir = path.join(this.config.outputDir, runName(this.config.stage, startedAt));
    fs.mkdirSync(runDir, { recursive: false });
    const paths = {
      runDir,
      metaDb: path.join(runDir, 'meta.sqlite'),
      metaLedger: path.join(runDir, 'meta.ledger'),
      projectionDb: path.join(runDir, 'rosetta.sqlite'),
      projectionLedger: path.join(runDir, 'rosetta.ledger'),
      report: path.join(runDir, 'benchmark-report.json'),
    };
    const report = {
      format: FORMAT,
      startedAt: startedAt.toISOString(),
      finishedAt: null,
      status: 'running',
      stage: this.config.stage,
      config: {
        binary: this.config.binary,
        home: this.config.home,
        agents: this.config.agents,
      },
      paths,
      phases: {},
    };
    this.emit({ type: 'run-start', report });

    try {
      if (this.config.stage === 'discovery') {
        await this.phase(report, 'discovery', 'discover', {
          home: this.config.home,
          agents: this.config.agents,
        });
        this.requireAgentEvents(report, 'discovery', 'discovery', (event) => event.installed);
      } else if (this.config.stage === 'meta') {
        await this.phase(report, 'meta', 'meta', {
          db: paths.metaDb,
          ledger: paths.metaLedger,
          home: this.config.home,
          agents: this.config.agents,
        });
        this.requireAgentEvents(report, 'meta', 'meta');
        if (!fs.existsSync(paths.metaDb)) {
          throw new Error('rosettad exited successfully without publishing meta.sqlite');
        }
      } else {
        await this.phase(report, 'ingest', 'ingest', {
          db: paths.projectionDb,
          ledger: paths.projectionLedger,
          home: this.config.home,
          agents: this.config.agents,
        });
        this.requireAgentEvents(report, 'ingest', 'stream');
        if (!fs.existsSync(paths.projectionDb)) {
          throw new Error('rosettad exited successfully without publishing rosetta.sqlite');
        }
      }
      report.status = 'complete';
      return report;
    } catch (error) {
      report.status = this.cancelled ? 'cancelled' : 'failed';
      report.error = serializeError(error);
      throw Object.assign(error, { benchmarkReport: report });
    } finally {
      report.finishedAt = new Date().toISOString();
      fs.writeFileSync(paths.report, `${JSON.stringify(report, null, 2)}\n`, { flag: 'wx' });
      this.emit({ type: 'run-finish', report });
      this.active = null;
    }
  }

  requireAgentEvents(report, phase, eventName, predicate = () => true) {
    const events = report.phases[phase]?.events || [];
    const missing = this.config.agents.filter(
      (agent) => !events.some((event) => event.event === eventName && event.agent === agent && predicate(event)),
    );
    if (missing.length) {
      throw new Error(`${phase} did not complete for selected agent${missing.length === 1 ? '' : 's'}: ${missing.join(', ')}`);
    }
  }

  async phase(report, phase, command, config) {
    if (this.cancelled) throw new Error('benchmark cancelled');
    const args = [command];
    if (config.db) args.push('--db', config.db);
    if (config.ledger) args.push('--ledger', config.ledger);
    args.push('--home', config.home);
    for (const agent of config.agents) args.push('--agent', agent);
    args.push('--profile', '--json');

    const events = [];
    const logs = [];
    const phaseReport = {
      command: [this.config.binary, ...args],
      status: 'running',
      wallMs: null,
      profile: {},
      events,
      logs,
    };
    report.phases[phase] = phaseReport;
    this.emit({ type: 'phase-start', phase, command: phaseReport.command });
    this.active = executeRosettad(this.config.binary, args, {
      onEvent: (event) => {
        events.push(event);
        this.emit({ type: 'daemon-event', phase, event });
      },
      onLog: (entry) => {
        if (logs.length >= 2_000) logs.shift();
        logs.push(entry);
        this.emit({ type: 'log', phase, ...entry });
      },
    });
    try {
      const result = await this.active.promise;
      phaseReport.status = 'complete';
      phaseReport.wallMs = result.durationMs;
      phaseReport.profile = profileFromEvents(events);
      this.emit({ type: 'phase-complete', phase, phaseReport });
    } catch (error) {
      phaseReport.status = this.cancelled ? 'cancelled' : 'failed';
      phaseReport.wallMs = error.details?.durationMs ?? null;
      phaseReport.profile = profileFromEvents(events);
      phaseReport.error = serializeError(error);
      this.emit({ type: 'phase-failed', phase, phaseReport });
      throw error;
    } finally {
      this.active = null;
    }
  }
}

module.exports = { BenchmarkRun, FORMAT, runName, serializeError, validateStage };
