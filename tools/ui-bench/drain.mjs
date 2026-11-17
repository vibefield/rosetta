// The daemon's dedicated stdout reader. rosettad writes stdout under the stdout lock,
// from the sole canonical writer among others, so a consumer that falls behind could
// stall the build. This process does nothing but read the daemon's pipes, stamp each
// line on receipt (epoch ms: timeOrigin + now()) and append it to a file; it forwards
// only the few small lines the driver steers by (ready, clock, milestones) over IPC.
//
// usage (from run_ui.mjs, with an IPC channel): node drain.mjs STDOUT_LOG STDERR_LOG -- BINARY ARGS...
// IPC in: {type:'stop', graceMs} → SIGINT to the daemon, then SIGKILL if it has not
// exited after graceMs (default 60 s). IPC out: {type:'line', t, line},
// {type:'spawned', pid, t}, {type:'exit', code, signal, t}. If the driver goes away
// (IPC disconnect) the daemon gets the same stop.
import { spawn } from 'node:child_process';
import { openSync, writeSync, closeSync } from 'node:fs';

const argv = process.argv.slice(2);
const dash = argv.indexOf('--');
const [stdoutPath, stderrPath] = argv.slice(0, dash);
const [binary, ...args] = argv.slice(dash + 1);
const now = () => performance.timeOrigin + performance.now();
const out = openSync(stdoutPath, 'wx', 0o600);
const err = openSync(stderrPath, 'wx', 0o600);
const send = (msg) => { try { if (process.connected) process.send(msg, () => {}); } catch { /* the driver left */ } };
// JSON events print with sorted keys, so match the event name anywhere in a short line.
const STEER = /^(milestone: |clock: )|"event":"(ready|clock|milestone)"/;

const child = spawn(binary, args, { stdio: ['ignore', 'pipe', 'pipe'] });
send({ type: 'spawned', pid: child.pid ?? null, t: now() });
function decoder(fd, forward) {
  let pending = '';
  return (chunk) => {
    const t = now();
    pending += chunk.toString('utf8');
    const lines = pending.split('\n');
    pending = lines.pop();
    let text = '';
    for (const line of lines) {
      if (!line) continue;
      text += `${t.toFixed(3)}\t${line}\n`;
      if (forward && line.length < 4096 && STEER.test(line)) send({ type: 'line', t, line });
    }
    if (text) writeSync(fd, text);
  };
}
const onOut = decoder(out, true);
const onErr = decoder(err, false);
child.stdout.on('data', onOut);
child.stderr.on('data', onErr);
child.stdout.on('end', () => onOut('\n'));
child.stderr.on('end', () => onErr('\n'));
let exited = false;
let killTimer = null;
function stop(graceMs = 60_000) {
  if (exited || killTimer !== null) return;
  child.kill('SIGINT');
  killTimer = setTimeout(() => { if (!exited) child.kill('SIGKILL'); }, graceMs);
}
process.on('message', (msg) => { if (msg?.type === 'stop') stop(Number(msg.graceMs) || undefined); });
process.on('disconnect', () => stop());
// A signal aimed at this process alone stops the daemon too, rather than orphaning it.
for (const sig of ['SIGINT', 'SIGTERM', 'SIGHUP']) process.on(sig, () => stop());
child.on('exit', () => { exited = true; if (killTimer !== null) clearTimeout(killTimer); });
child.on('close', (code, signal) => {
  closeSync(out);
  closeSync(err);
  send({ type: 'exit', code, signal, t: now() });
  setTimeout(() => process.exit(0), 50);
});
