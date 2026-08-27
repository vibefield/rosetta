const { spawn } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');

const AGENTS = new Set([
  'antigravity',
  'claude-code',
  'codex',
  'copilot',
  'grok',
  'hermes',
  'kimi-code',
  'opencode',
  'pi',
]);

class DaemonError extends Error {
  constructor(message, details = {}) {
    super(message);
    this.name = 'DaemonError';
    this.details = details;
  }
}

function assertFile(file, label) {
  if (typeof file !== 'string' || !file.trim()) {
    throw new DaemonError(`${label} is required`);
  }
  const resolved = path.resolve(file);
  let stat;
  try {
    stat = fs.statSync(resolved);
  } catch (error) {
    throw new DaemonError(`${label} does not exist: ${resolved}`, { cause: error.message });
  }
  if (!stat.isFile()) {
    throw new DaemonError(`${label} is not a file: ${resolved}`);
  }
  return resolved;
}

function assertDirectory(directory, label, { create = false } = {}) {
  if (typeof directory !== 'string' || !directory.trim()) {
    throw new DaemonError(`${label} is required`);
  }
  const resolved = path.resolve(directory);
  if (create) fs.mkdirSync(resolved, { recursive: true });
  let stat;
  try {
    stat = fs.statSync(resolved);
  } catch (error) {
    throw new DaemonError(`${label} does not exist: ${resolved}`, { cause: error.message });
  }
  if (!stat.isDirectory()) {
    throw new DaemonError(`${label} is not a directory: ${resolved}`);
  }
  return resolved;
}

function validateAgents(agents, { allowEmpty = false } = {}) {
  if (!Array.isArray(agents)) throw new DaemonError('agents must be an array');
  const unique = [...new Set(agents)];
  if (!allowEmpty && unique.length === 0) throw new DaemonError('select at least one agent');
  for (const agent of unique) {
    if (!AGENTS.has(agent)) throw new DaemonError(`unknown agent kind: ${agent}`);
  }
  return unique;
}

function parseProfile(report) {
  if (typeof report !== 'string') return {};
  const metrics = {};
  for (const line of report.split(/\r?\n/)) {
    const match = line.match(/^(.+?)\s+(-?\d+(?:\.\d+)?)(?:\s+(ms|MiB\/s|records\/s|ops\/s))?$/);
    if (!match) continue;
    metrics[match[1].trim()] = {
      value: Number(match[2]),
      unit: match[3] || 'count',
    };
  }
  return metrics;
}

function argsFor(command, config = {}) {
  const args = [command];
  if (config.db) args.push('--db', path.resolve(config.db));
  if (config.ledger) args.push('--ledger', path.resolve(config.ledger));
  if (config.home) args.push('--home', path.resolve(config.home));
  for (const agent of validateAgents(config.agents || [], { allowEmpty: true })) {
    args.push('--agent', agent);
  }
  if (config.profile) args.push('--profile');
  if (config.json) args.push('--json');
  return args;
}

function lineDecoder(onLine) {
  let pending = '';
  return {
    push(chunk) {
      pending += chunk.toString('utf8');
      const lines = pending.split(/\r?\n/);
      pending = lines.pop() || '';
      for (const line of lines) if (line) onLine(line);
    },
    end() {
      if (pending) onLine(pending);
      pending = '';
    },
  };
}

function executeRosettad(binary, args, { onEvent = () => {}, onLog = () => {} } = {}) {
  const executable = assertFile(binary, 'rosettad binary');
  const started = process.hrtime.bigint();
  let child;
  let stderrTail = '';
  let stdoutTail = '';
  let settled = false;

  const promise = new Promise((resolve, reject) => {
    child = spawn(executable, args, {
      stdio: ['ignore', 'pipe', 'pipe'],
      windowsHide: true,
    });
    const consume = (stream, line) => {
      onLog({ stream, line });
      if (stream === 'stdout') {
        stdoutTail = `${stdoutTail}${line}\n`.slice(-128_000);
        try {
          onEvent(JSON.parse(line));
        } catch {
          // Human output is still useful in the log pane.
        }
      } else {
        stderrTail = `${stderrTail}${line}\n`.slice(-128_000);
      }
    };
    const stdout = lineDecoder((line) => consume('stdout', line));
    const stderr = lineDecoder((line) => consume('stderr', line));
    child.stdout.on('data', (chunk) => stdout.push(chunk));
    child.stderr.on('data', (chunk) => stderr.push(chunk));
    child.once('error', (error) => {
      if (settled) return;
      settled = true;
      reject(new DaemonError(`could not start rosettad: ${error.message}`, { cause: error.message }));
    });
    child.once('close', (code, signal) => {
      stdout.end();
      stderr.end();
      if (settled) return;
      settled = true;
      const durationMs = Number(process.hrtime.bigint() - started) / 1e6;
      const result = { code, signal, durationMs, stdoutTail, stderrTail };
      if (code === 0) resolve(result);
      else {
        let reason;
        if (signal) {
          reason = `rosettad stopped by ${signal}`;
        } else if (/unknown flag --json/i.test(stderrTail)) {
          reason = 'This rosettad binary predates the ingest-bench JSON protocol. Restart with npm start to rebuild it, or choose a freshly built binary.';
        } else {
          reason = stderrTail.trim().split(/\r?\n/).pop() || `rosettad exited with code ${code}`;
        }
        reject(new DaemonError(reason, result));
      }
    });
  });

  return {
    promise,
    cancel() {
      if (child && child.exitCode === null && child.signalCode === null) child.kill('SIGTERM');
    },
  };
}

async function discover(binary, home, { agents = [], onEvent, onLog } = {}) {
  const events = [];
  const command = executeRosettad(
    binary,
    argsFor('discover', { home, agents, json: true }),
    {
      onEvent(event) {
        events.push(event);
        onEvent?.(event);
      },
      onLog,
    },
  );
  const processResult = await command.promise;
  return {
    process: processResult,
    discoveries: events.filter((event) => event.event === 'discovery'),
    profile: profileFromEvents(events),
  };
}

function profileFromEvents(events) {
  const profile = events.findLast((event) => event.event === 'profile');
  return profile ? parseProfile(profile.report) : {};
}

module.exports = {
  AGENTS,
  DaemonError,
  argsFor,
  assertDirectory,
  assertFile,
  discover,
  executeRosettad,
  parseProfile,
  profileFromEvents,
  validateAgents,
};
