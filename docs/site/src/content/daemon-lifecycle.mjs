import { code, table, note, section as s, sourceUrl } from '../html.mjs';

export const daemonLifecycle = {
  id: 'daemon-lifecycle',
  title: 'Daemon ownership & recovery',
  group: 'Design proposals',
  description: 'A production design for process supervision, private shutdown, HTTP controls, and recovery across both desktop hosts.',
  source: 'docs/DAEMON_LIFECYCLE_PROPOSAL.md',
  render: ({ url, version }) => `
    ${note('Proposal · not implemented', `This page defines the proposed process contract. Supervised-stdio mode, the health route, and the new identity fields are not implemented in protocol ${version}. Existing routes remain in the <a href="${url('reference')}">API reference</a>.`)}
    ${s('ownership', 'One owner. Three responsibilities.', `<p>Fieldd owns one foreground rosettad process per configured index. The standalone app’s Electron main owns its own instance. The shared History room gets observations and permitted controls through host ports.</p>${table(['Responsibility', 'Channel', 'Owner'], [
      ['Process lifetime', 'Spawn, private shutdown pipe, verified OS exit.', 'fieldd or Electron main.'],
      ['Ingestion control', 'Typed HTTP/JSON commands and status.', 'Rosetta’s cold/live coordinator.'],
      ['History reads', 'Existing HTTP and WebSocket subscriptions.', 'Host adapters and scoped session clients.'],
    ])}<p>A <a href="${url('ingestion-lifecycle')}">cold run</a> and the <a href="${url('live-ingestion')}">live controller</a> survive process recovery through their durable intent. Restarting a process does not rebuild the archive or change a user’s pause.</p><p>Keep the current app lifetimes. An optional later background deployment can put fieldd under an OS service manager; each rosettad still has one owner.</p>`)}
    ${s('identity', 'Give each lifetime its own identity', table(['Identity', 'Lifetime', 'Consumer rule'], [
      ['<code>instance_id</code>', 'One spawn attempt.', 'Reject stale ready events, replies, and process controls.'],
      ['<code>index_id</code>', 'One durable logical index.', 'Bind mutation intent across process restarts.'],
      ['<code>projection_generation</code>', 'One published archive generation.', 'Invalidate generation-bound reads and caches.'],
      ['<code>ingestion_id</code>', 'One cold run, including pause and recovery.', 'Reconcile commands and observed work.'],
      ['<code>progress.epoch</code>', 'One comparable RAM observation period.', 'Reset displayed rates when observations change.'],
    ]) + '<p>The host replaces origin, instance id, and credential atomically. A new process can keep the same logical index; an archive publication can keep the same process.</p>')}
    ${s('startup', 'Start the control service before ingestion', `${code('--supervised-stdio --instance-id <fresh UUID> --json', 'Proposed additions to serve')}<p>Spawn the binary directly with piped stdin, stdout, and stderr. Install the owner channel and shutdown handlers before expensive work, acquire the exclusive index lock, then bind the control service.</p><p>The structured ready event adds the instance id, private supervisor protocol version, and capabilities. Validate those alongside the current API version, loopback origin, and required authentication. Confirm the same instance through the proposed health endpoint before exposing a connection.</p><p>Ready means the control service is available. It does not wait for archive ingestion or search to finish. A paused import leaves the process ready.</p>`)}
    ${s('shutdown', 'Use a private channel for process shutdown', `${code(JSON.stringify({
      supervisor_protocol: 1,
      command: 'shutdown',
      instance_id: 'c6fb255a-c847-4c6d-94b5-29cb3cfc9cd2',
      request_id: '7ffea3f0-c24c-47e8-8da9-b7ed272aeb53',
      reason: 'enrollment_changed',
      grace_ms: 10000,
    }, null, 2), 'Proposed stdin frame · one JSON line on the wire')}<p>Only the supervisor owns the pipe’s write handle. This channel supports process shutdown and detects owner loss through EOF; ingestion commands stay on HTTP. There is no new HTTP shutdown endpoint.</p><p>Shutdown closes write admission, stops capture at recoverable boundaries, syncs required evidence, joins workers, closes streams, and releases the lock last. It preserves accepted running, paused, and discard intent. The next launch recovers that intent.</p>${note('Portable graceful shutdown', 'Use the pipe on Windows, macOS, and Linux. Unix SIGTERM and SIGINT enter the same drain path. Node’s child.kill("SIGINT") is abrupt on Windows, so it is only a forced-termination mechanism there. Plain CLI serve does not interpret stdin EOF as shutdown.')}`)}
    ${s('exit', 'A timeout is not an exit', `<p>The default stop budget is 10 seconds to drain, followed by forced termination and up to 2 seconds to confirm exit. A shutdown acknowledgement only confirms draining. Only OS exit evidence completes ownership.</p><p>If exit remains unconfirmed, retain the child handle and instance id, report <code>exit_unconfirmed</code>, and refuse a replacement. Keep listening for the real exit. An announcement file or a reachable endpoint never authorizes killing an unverified PID.</p>${table(['Policy', 'Initial default'], [
      ['Startup and health confirmation', '8 seconds per attempt; one bounded lock-handoff retry, 30 seconds overall.'],
      ['Crash recovery', 'Jittered exponential backoff, capped at 10 seconds.'],
      ['Retry intensity', 'At most 5 automatic retries in 60 seconds.'],
      ['Stable period', '30 seconds before resetting the backoff exponent.'],
      ['Permanent failure', 'Wrong binary, invalid configuration, or unresolved ownership requires explicit recovery.'],
    ])}<p>Serialize ensure, stop, and restart. A winning stop cancels queued retries; late callbacks cannot make the old connection usable again.</p>`)}
    ${s('health', 'Observe process health separately', `${code('GET /api/health\n\n' + JSON.stringify({
      instance_id: 'c6fb255a-c847-4c6d-94b5-29cb3cfc9cd2',
      state: 'ready',
      accepting_mutations: true,
    }, null, 2), 'Proposed health response')}<p>This daemon-level read follows the existing access rules and is unavailable to session grants. It performs no SQLite work or source scan. During draining, state becomes <code>draining</code> and admission becomes false; liveness can still return HTTP 200.</p><p>One host monitor probes every 5 seconds with a 2-second timeout. Three consecutive transport failures for the same instance can enter bounded recovery. Reset strikes on system resume. Quiet sources, paused ingestion, and incomplete search do not trigger process restart.</p><p>Status adds the same process lifecycle projection beside the proposed ingestion overview. The latter owns canonical readiness, cold/live state, and action eligibility.</p>`)}
    ${s('commands', 'Keep a retried command on the same index', `${code('POST /api/ingestions/ing_example/pause\nContent-Type: application/json\nX-Rosetta-Index-Id: idx_example\nIdempotency-Key: 77e9a008-d177-48c7-ac4b-b75adfdc5a6c\n\n{"expected_revision":7}', 'Proposed HTTP')}<p>The index precondition comes from <code>status.ingestion.index_id</code>. Validate it before reading idempotency receipts; a mismatch returns <code>409 index_changed</code> without work. It is a target precondition, not an authentication credential.</p><p>After a lost response or process restart, retry with the same index, operation key, revision, and replacement generation. A deliberate user or index switch cancels that pending intent. Instance-aware connection updates reject old replies, while projection generations invalidate archive caches.</p>`)}
    ${s('hosts', 'Preserve the host boundaries', table(['Host surface', 'Authority'], [
      ['Vibefield ingestion controls', 'Audited local fieldd RPCs under index.manage; fieldd holds the bearer.'],
      ['Vibefield process restart', 'Existing system.index.restart under native.admin; reconcile health after an uncertain reply.'],
      ['Standalone controls', 'Owned SDK client; Electron main alone holds the process channel.'],
      ['Session readers', 'Existing session grants; no daemon lifecycle access.'],
    ]) + '<p>Enrollment remains an atomic file update followed by supervised restart in Vibefield. Preserve paused intent and use the revision actually loaded. The standalone open-local compatibility profile remains explicit; the new process channel adds no public shutdown authority.</p>')}
    ${s('delivery', 'Deliver in verifiable stages', table(['Stage', 'Gate'], [
      ['Process foundation', 'Real process tests for Windows, macOS, and Linux; pipe EOF, signals, verified exit, and bounded retries.'],
      ['Ingestion controls', 'Coordinator and both host adapters; lost replies, identity checks, paused recovery, and cleanup.'],
      ['Archive replacement', 'Retained history, coherent publication, both read planes, and cache invalidation.'],
      ['Optional service packaging', 'Explicit account/session lifetime, upgrades, logout/reboot, and platform ownership.'],
    ]) + `<p>Vibefield requires a coordinated upgrade of its pinned Rosetta binary and packages, schemas, contracts, and adapters. Keep proposed routes outside the current endpoint inventory until they ship.</p><p><a href="${sourceUrl('docs/DAEMON_LIFECYCLE_PROPOSAL.md')}">Read the complete design, state machine, budgets, and acceptance matrix</a>. The <a href="${sourceUrl('docs/INGESTION_API_REVIEW.md')}">integration review</a> records the consumer evidence.</p>`)}
  `,
};
