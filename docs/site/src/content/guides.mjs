import { code, tabs, table, note, section as s, icon, sourceUrl } from '../html.mjs';
import { searchExample } from './examples.mjs';
import { ingestionLifecycle } from './ingestion-lifecycle.mjs';
import { liveIngestion } from './live-ingestion.mjs';
import { daemonLifecycle } from './daemon-lifecycle.mjs';

export const guides = [
  {
    id: '', title: 'Overview', group: 'Start here', description: 'A local API for searching, reading, and following agent conversations.', source: 'README.md',
    render: ({ url, version, count, typeCount }) => `
      <div class="overview-intro"><div class="eyebrow">ROSETTA / DEVELOPER DOCUMENTATION</div><h1>Agent history.<br><span>One shared language.</span></h1><p class="lead">Read conversations, search across agents, and follow changes live. Rosetta turns local agent data into one consistent API.</p><div class="intro-actions"><a class="button primary" href="${url('quickstart')}">Start building ${icon('chevron')}</a><a class="text-action" href="${url('reference')}">Explore the API ${icon('chevron')}</a></div></div>
      <div class="contract-strip"><span><b>${count}</b> endpoints</span><span><b>${typeCount}</b> generated types</span><span>Protocol <b>${version}</b></span><span>Local-first</span></div>
      ${s('first-request', 'A familiar place to start', `<p>One daemon. Plain HTTP. Start with the session catalog, then open a conversation by its canonical id.</p>${tabs([['cURL', 'curl "http://127.0.0.1:4640/api/sessions?limit=20"'], ['TypeScript', 'import { RosettaClient } from "@vibefield/rosetta-protocol";\n\nconst client = new RosettaClient();\nconst { sessions } = await client.sessions({ limit: 20 });\n\nconsole.log(sessions);']], 'first', { dark: true, caption: 'List recent sessions' })}<p class="small">Default origin <code>http://127.0.0.1:4640</code> <span class="separator">·</span> <a href="${url('quickstart')}">Run your first daemon</a></p>`)}
      ${s('one-contract', 'From many formats to one contract', `<div class="pipeline" aria-label="Agent files flow through Rosetta into REST, WebSocket, and TypeScript consumers"><div class="pipeline-input"><span class="eyebrow">LOCAL AGENT DATA</span><strong>Claude Code · Codex · Copilot</strong><span>and six more adapters</span></div><span class="pipeline-connector" aria-hidden="true">→</span><div class="pipeline-core">${icon('layers')}<strong>Rosetta</strong><span>Canonical record</span></div><span class="pipeline-connector" aria-hidden="true">→</span><div class="pipeline-output"><span>REST <small>Read & search</small></span><span>WebSocket <small>Follow changes</small></span><span>TypeScript <small>Build a client</small></span></div></div><p class="small">Adapters own native formats. Your application consumes the service contract.</p>`)}
      ${s('build', 'Find your starting point', `<div class="guide-grid">${[
        ['quickstart', 'terminal', 'Make your first request', 'Build the daemon, choose agents, and read a session.'],
        ['reference', 'code', 'Browse the API', 'Every route, parameter, response type, and example.'],
        ['subscriptions', 'pulse', 'Follow a conversation', 'Snapshots, replay, and consistent live history.'],
        ['typescript', 'package', 'Use the TypeScript client', 'Let the SDK own transport and subscription state.'],
      ].map(([path, glyph, title, description]) => `<a class="guide-card" href="${url(path)}">${icon(glyph)}<h3>${title} ${icon('chevron')}</h3><p>${description}</p></a>`).join('')}</div>`)}
      ${s('supported-agents', 'Nine adapters. One vocabulary.', `<p class="agent-list">Antigravity <span>·</span> Claude Code <span>·</span> Codex <span>·</span> Copilot <span>·</span> Grok <span>·</span> Hermes <span>·</span> Kimi Code <span>·</span> OpenCode <span>·</span> Pi</p><p>Each adapter normalizes its own evidence into projects, sessions, and timeline entries. Search currently indexes user prompts and agent replies. A dedicated metrics stream is still planned.</p>`)}
    `,
  },
  {
    id: 'quickstart', title: 'Quickstart', group: 'Start here', description: 'Go from a checkout to your first session in four steps.', source: 'crates/rosettad/src/main.rs',
    render: ({ url, version, rustVersion }) => `
      ${s('build-daemon', '1. Build the daemon', `<p>From the repository root, build with the pinned Rust toolchain (${rustVersion}). The repository is private; use a checkout you have access to.</p>${code('cargo build --release --locked -p rosettad', 'Shell')}<p>The binary is in Cargo’s target directory under <code>release/rosettad</code>. The commands below use <code>cargo run</code> so a local target-directory override also works.</p>`)}
      ${s('start-daemon', '2. Choose what to ingest', `<p>Start a projection in a writable data directory. This command explicitly enrolls the installed Claude Code and Codex roots.</p>${code('mkdir -p "$HOME/.local/share/rosetta"\ncargo run --release --locked -p rosettad -- serve \\\n  --db "$HOME/.local/share/rosetta/history.sqlite" \\\n  --agent claude-code --agent codex', 'Shell')}<p>Substitute the agent kinds you use. <code>--all-agents</code> opts into every installed agent. With no enrollment on a fresh start, the daemon waits for a choice through <a href="${url('reference/enable-runtimes')}">the consent endpoint</a>.</p>${note('Keep the daemon running', 'The default listener is <code>127.0.0.1:4640</code>. It binds before ingestion completes, and keeps watching for changes. Use a second terminal for requests. Your projection, sibling ledger, and assets are local data; keep them out of version control.')}`)}
      ${s('check-readiness', '3. Check readiness', `<p>Inspect the running daemon and the wire version before making assumptions about the available data.</p>${code('curl -i http://127.0.0.1:4640/api/status', 'Shell')}<p>The response header <code>x-rosetta-protocol-version: ${version}</code> identifies this contract. Session metadata arrives progressively. Wait for <code>canonical_ready: true</code> before reading project aggregates, and inspect <code>search.readiness</code> separately.</p><p><a href="${url('readiness')}">Understand the readiness states ${icon('chevron')}</a></p>`)}
      ${s('read-session', '4. Open a conversation', `${code('curl "http://127.0.0.1:4640/api/sessions?limit=20"\n\n# Replace SESSION_ID with an id from the response.\ncurl "http://127.0.0.1:4640/api/sessions/SESSION_ID/timeline?limit=100"', 'Shell')}<p>Timeline pages arrive oldest first within the latest window. Pass the returned <code>cursor</code> to fetch older entries, or use a <a href="${url('subscriptions')}">live subscription</a> to follow the conversation.</p>${note('Connecting to a configured daemon', `Plain serve is open to processes that can reach its listener. If the daemon uses <code>--token-file</code>, send <code>Authorization: Bearer &lt;token&gt;</code>. See <a href="${url('authentication')}">authentication and session grants</a>.`)}`)}
    `,
  },
  {
    id: 'architecture', title: 'How Rosetta fits together', group: 'Start here', description: 'The boundaries that keep nine agent formats behind one stable service contract.', source: 'docs/ARCHITECTURE.md',
    render: ({ url }) => `
      ${s('layers', 'Three layers, one direction', table(['Layer', 'Responsibility', 'Source'], [
        ['Ground truth', 'Data-only evidence packs and Python tooling that describe each agent’s real formats.', '<code>groundtruth/</code><br><code>tools/groundtruth/</code>'],
        ['Runtime', 'Capture, native adapters, normalization, the raw ledger, and canonical storage.', '<code>crates/{capture,adapters,core,pipeline,store}</code>'],
        ['Services', 'Backend-neutral wire types, HTTP, WebSocket, and generated clients.', '<code>crates/{protocol,server}</code><br><code>clients/ts/</code>'],
      ]) + '<p>Consumers use layer 3. Native file layouts and parser quirks remain in each adapter; client applications do not read SQLite or parse agent transcripts directly.</p>')}
      ${s('storage', 'Evidence and projection', `<p>The append-only raw ledger is the replay source. SQLite and content-addressed assets form the canonical projection. The projection is disposable and rebuilt from evidence; schema mismatches fail closed instead of silently migrating.</p><p>Search uses a detached, disposable FTS5 sidecar. It has its own epoch, revision, and readiness, so a usable transcript does not imply a complete search index.</p>${note('The persistence boundary', 'Canonical storage holds user-facing facts. Native payloads, incomplete tool fragments, and reconciliation machinery do not become a second persisted model.')}`)}
      ${s('entities', 'The objects you build with', table(['Object', 'Meaning'], [
        [`<a href="${url('types/RuntimeRow')}">Runtime</a>`, 'One agent family at one data root. Several roots can belong to the same family.'],
        [`<a href="${url('types/ProjectRow')}">Project</a>`, 'One proven local checkout group. Linked worktrees converge; separate clones do not.'],
        [`<a href="${url('types/SessionRow')}">Session</a>`, 'A conversation with one primary cwd and an optional project. Children retain their own canonical ids.'],
        [`<a href="${url('types/TimelineEntry')}">Timeline entry</a>`, 'A canonical user, agent, tool, reasoning, collaboration, or other recorded event, keyed by timeline_id.'],
        [`<a href="${url('types/TimelineMeta')}">Timeline metadata</a>`, 'A light work index of tasks, workflows, plans, and artifacts. Bodies stay behind lazy references.'],
      ]))}
      ${s('codebase', 'Where to work', table(['If you are changing…', 'Start here'], [
        ['A REST route', '<code>crates/server/src/http.rs</code>'],
        ['A subscription', '<code>crates/server/src/ws.rs</code> and <code>hub.rs</code>'],
        ['A wire shape', '<code>crates/protocol/src/lib.rs</code> and its re-exported core types'],
        ['Client behavior', '<code>clients/ts/client.ts</code>'],
        ['A History room integration', '<code>clients/room/</code> (injected ports; the host owns connections)'],
        ['An agent adapter', 'Read <code>docs/groundtruth/RESEARCH_PLAN.md</code>, then the agent’s canonical ground-truth pack.'],
      ]))}
      ${s('contribute', 'Keep the contract aligned', `<p>Breaking route or frame changes bump Rust’s <code>PROTOCOL_VERSION</code> and <code>clients/ts/protocol-version.mjs</code> together. Regenerate the TypeScript bindings and refresh the wire golden.</p>${code('TS_RS_EXPORT_DIR="$PWD/clients/ts/generated" TS_RS_LARGE_INT=number \\\n  cargo test -p rosetta-protocol --features ts\n\nnode docs/site/scripts/build.mjs --check', 'Shell')}<p>Use a feature branch and pull request. The full repository checks are documented in the <a href="${sourceUrl('README.md')}">README</a>; the complete design constraints live in <a href="${sourceUrl('docs/ARCHITECTURE.md')}">ARCHITECTURE.md</a>.</p>`)}
    `,
  },
  {
    id: 'authentication', title: 'Authentication & grants', group: 'Essentials', description: 'Connect with a bearer credential, or grant access to one session’s recorded history.', source: 'docs/AUTH.md',
    render: ({ url }) => `
      ${s('modes', 'Choose the daemon’s access mode', table(['Serve flags', 'Access'], [
        ['None', 'Open to processes that reach the listener. The browser Origin rule still applies.'],
        ['<code>--token-file FILE</code>', 'A credential is required on every route and WebSocket upgrade.'],
        ['<code>--token-file FILE --grant-key-file FILE</code>', 'Bearer access plus signed, session-scoped grants. A grant key without a bearer is refused.'],
      ]) + '<p>The token file contains 32–1,024 visible ASCII characters, trimmed at its boundaries. The key file contains at least 32 raw bytes, without trimming or decoding. Both must be regular files no larger than 4 KiB; Unix permissions must be <code>0600</code> or stricter. They are read once, before the listener binds.</p>')}
      ${s('bearer', 'Present a credential', `${tabs([['cURL', 'curl http://127.0.0.1:4640/api/status \\\n  -H "Authorization: Bearer $ROSETTA_TOKEN"'], ['TypeScript', 'const client = new RosettaClient({\n  credential: { kind: "bearer", token },\n});\n\nconst status = await client.status();']], 'auth')}<p>A session grant uses <code>Authorization: Rosetta-Grant &lt;grant&gt;</code> or <code>{ kind: "grant", grant }</code> in the SDK. The SDK’s <code>setCredential()</code> supplies a refreshed credential on future requests and reconnects.</p>`)}
      ${s('scope', 'Session grants have an exact reach', `<p>A grant binds <code>sid</code> to one canonical session, <code>cid</code> to a client label, and <code>exp</code> to an expiry in Unix milliseconds. It is HMAC-SHA256 over canonical version-1 JSON. Mint it in the trusted Node supervisor through <code>@vibefield/rosetta-protocol/grants</code>.</p>${table(['Scope', 'Opens'], [
        ['<code>read</code>', 'The exact session, timeline, collaboration history, lazy command/work/plan reads, session and collaboration sockets, and known projection assets.'],
        ['<code>read</code> + <code>files</code>', 'Also the current file named by a recorded timeline target.'],
        ['Bearer only', 'Catalog lists, metadata socket, status, discoveries, search, pricing, aggregates, and both writes.'],
      ])}${note('History and current files are different permissions', 'A file named in a transcript may contain newer or different data today. The files scope is explicit. A child session also needs its own grant; grants do not widen to descendants.')}`)}
      ${s('websocket-auth', 'Authenticate a browser WebSocket', `${code('import { webSocketProtocols } from "@vibefield/rosetta-protocol";\n\nconst socket = new WebSocket(\n  `ws://127.0.0.1:4640/ws/sessions/${encodeURIComponent(sessionId)}`,\n  webSocketProtocols({ kind: "grant", grant }),\n);', 'TypeScript')}<p>The protocol pair is <code>rosetta</code> plus <code>rosetta.bearer.&lt;base64url(token)&gt;</code> or <code>rosetta.grant.&lt;grant&gt;</code>. The server selects only <code>rosetta</code>. Authorization headers take precedence if both carriers are present.</p><p>Credentials are checked at upgrade. An open socket can outlive a grant’s expiry; its next connection needs a valid grant. The SDK handles the carrier automatically.</p>`)}
      ${s('origins', 'Browser origins are a separate check', `<p>Built-in origins are HTTP/HTTPS on <code>localhost</code>, <code>127.0.0.1</code>, and <code>[::1]</code> at any port, plus <code>tauri://localhost</code> and <code>rosetta://app</code>. Repeat <code>--allow-origin scheme://host[:port]</code> to admit another exact origin.</p><p>Null origins, wildcards, paths, credentials, and queries are refused. Requests without an Origin still need a credential when bearer mode is configured. WebSocket upgrades, writes, and file reads explicitly reject foreign origins with 403.</p>`)}
      ${s('refusals', 'Handle a refused credential', `<p>A 401 is plain text, with <code>x-rosetta-auth</code>, <code>WWW-Authenticate</code>, and <code>Cache-Control: no-store</code>. The SDK throws <code>UnauthorizedError</code>; inspect its <code>reason</code>.</p>${table(['Reason', 'Next action'], [
        ['<code>missing</code> / <code>bad-token</code>', 'Supply the bearer for this daemon.'],
        ['<code>malformed</code> / <code>bad-signature</code>', 'Check the carrier and minting key; send one canonical credential.'],
        ['<code>expired</code>', 'Obtain a fresh grant from the supervisor.'],
        ['<code>wrong-session</code> / <code>wrong-scope</code>', 'Use a grant for this exact session and required scopes.'],
        ['<code>wrong-route</code> / <code>grants-disabled</code>', 'Use the bearer or enable grants on the daemon.'],
      ])}<p>The full format, verification order, and threat model are in <a href="${sourceUrl('docs/AUTH.md')}">AUTH.md</a>.</p>`)}
    `,
  },
  {
    id: 'readiness', title: 'Readiness & lifecycle', group: 'Essentials', description: 'Show what is available without turning an incomplete result into an empty one.', source: 'crates/protocol/src/lib.rs',
    render: ({ url }) => `
      ${s('two-gates', 'Two independent readiness gates', `<p><code>/api/status</code> exposes <code>canonical_ready</code> and <code>search.readiness</code>. The canonical projection can be readable while the detached search index is still building.</p>${table(['Surface', 'During a fresh cold start'], [
        ['Projects and sessions', 'Metadata appears as bounded runtime skims commit.'],
        ['Timeline', 'A pending runtime exposes no partial payload. Rows become visible when its readiness commit admits the runtime.'],
        ['Heatmap, usage, day usage, contribution', '503 with not_ready until canonical publication.'],
        ['Search', '200 with building readiness and no hits until canonical publication; then the detached index builds.'],
      ])}`)}
      ${s('search-states', 'Read the search state with every result', table(['Readiness', 'Meaning for the reader'], [
        ['<code>ready</code>', 'The index is available at the reported projection epoch and revision.'],
        ['<code>building</code>', 'The index is not yet published. Show progress when provided.'],
        ['<code>catching-up</code>', 'The index is repairing committed changes; do not imply complete coverage.'],
        ['<code>stale</code>', 'The reported index state is no longer current; do not imply complete coverage.'],
        ['<code>failed</code>', 'Search is unavailable. Keep transcript and metadata reads usable.'],
      ]) + `<p><a href="${url('types/SearchState')}">SearchState</a> carries projection identity and optional progress. An empty hits array only means “no matches” when the accompanying state supports that conclusion.</p>`)}
      ${s('session-state', 'Indexing is not agent liveness', `<p><code>SessionRow.stream_state</code> is <code>pending</code>, <code>streaming</code>, or <code>indexed</code>. It says how much recorded history is admitted, never whether an agent process is running.</p><p><code>activity</code> is recorded conversation time. <code>updated_at</code> is metadata freshness. Neither starts a wall-clock timer or proves an agent is currently working.</p>`)}
      ${s('recovery', 'Let the normal serve path recover', `<p>A fresh daemon builds in shared memory while publishing metadata, durably records evidence, then atomically installs the validated projection. Plain <code>serve</code> resumes an interrupted generation and performs cursor-relative catch-up for an existing complete projection.</p><p><code>--restart-incomplete</code> is an explicit legacy archive/restart operation, not the normal startup path. A schema mismatch requires an explicit rebuild from the ledger.</p>`)}
    `,
  },
  {
    id: 'pagination', title: 'Pagination & consistency', group: 'Essentials', description: 'Page backward without mixing timeline generations.', source: 'clients/ts/client.ts',
    render: ({ url }) => `
      ${s('cursor', 'Use the returned cursor', `<p>The latest page contains up to <code>limit</code> entries, oldest first. Its cursor points to the next older page; null means there is no further continuation. Treat the string as opaque.</p>${code('const latest = await client.timeline(sessionId, { limit: 100 });\n\nif (latest.cursor !== null) {\n  const older = await client.timeline(sessionId, {\n    cursor: latest.cursor,\n    limit: 100,\n  });\n  console.log([...older.entries, ...latest.entries]);\n}', 'TypeScript')}`)}
      ${s('watermark', 'A watermark binds the generation', `<p>Every timeline page and exact entry response carries a <code>watermark</code>. A cursor binds its session and watermark. When that generation is replaced, the cursor expires with HTTP 410 and <code>cursor_expired</code>.</p>${code('import { CursorExpiredError } from "@vibefield/rosetta-protocol";\n\ntry {\n  const older = await client.timeline(sessionId, { cursor });\n  console.log(older.entries);\n} catch (error) {\n  if (!(error instanceof CursorExpiredError)) throw error;\n  // Discard all cached pages, then start from the latest window.\n  const latest = await client.timeline(sessionId);\n  console.log(latest.entries);\n}', 'TypeScript')}`)}
      ${s('live-history', 'Keep live history inside the subscription', `<p><code>SessionSubscription.fetchPage()</code> retains old rows under the snapshot’s generation. <code>reveal()</code> adds their latest patched values to the visible timeline. <code>loadEntry()</code> fetches an exact anchor into the same cache.</p><p>Replacement snapshots invalidate loaded pages, anchors, and in-flight reads together. Use <code>onReset</code> to clear view state tied to the former generation. See <a href="${url('subscriptions')}">live subscriptions</a>.</p>${note('Only timelines use this cursor', 'The session catalog has a bounded limit, and search has a bounded hit count. Neither response exposes timeline-style continuation.')}`)}
    `,
  },
  {
    id: 'errors', title: 'Errors & compatibility', group: 'Essentials', description: 'Recognize protocol errors and recover at the right boundary.', source: 'crates/server/src/http.rs',
    render: ({ version }) => `
      ${s('wire-version', 'Check the wire version', `<p>Every routed HTTP response includes <code>x-rosetta-protocol-version</code>. Every WebSocket starts with <code>hello.protocol_version</code>. This checkout speaks protocol <strong>${version}</strong>.</p><p>The SDK rejects missing or incompatible versions with <code>incompatible_protocol</code>. Run a matching daemon and client before retrying. The package release version and the wire version are different numbers.</p>`)}
      ${s('error-shape', 'Service errors share an envelope', `${code('{\n  "error": {\n    "code": "cursor_expired",\n    "message": "the timeline was rewritten; refetch from the head"\n  }\n}', 'JSON')}${table(['Status / code', 'Recovery'], [
        ['400 / <code>bad_request</code>', 'Correct the request. Check query combinations, limits, and required bodies.'],
        ['404 / <code>not_found</code>', 'The requested session, row, or content is not available.'],
        ['410 / <code>cursor_expired</code>', 'Discard the former generation and refetch the latest timeline page.'],
        ['503 / <code>not_ready</code>', 'Wait for canonical publication before reading aggregates.'],
        ['500 / <code>internal</code>', 'Inspect the daemon error and retry only after addressing the underlying failure.'],
      ])}<p>Extractor rejections, an unmatched route, and Origin refusals need not use this JSON envelope. Authentication failures are 401 plain text with <code>x-rosetta-auth</code>.</p>`)}
      ${s('client-errors', 'Use typed client errors', `<p>The SDK exposes <code>ApiRequestError</code> with <code>status</code> and nullable <code>code</code>, <code>CursorExpiredError</code> for the timeline barrier, and <code>UnauthorizedError</code> with a nullable <code>reason</code>.</p><p>Requests time out after 30 seconds by default and accept an AbortSignal. A canceled request is distinct from an API error. <code>setBaseUrl()</code> aborts old-origin requests and reconnects subscriptions.</p>`)}
    `,
  },
  {
    id: 'typescript', title: 'TypeScript client', group: 'Build a client', description: 'One transport and subscription implementation, backed by generated wire types.', source: 'clients/ts/README.md',
    render: ({ url }) => `
      ${s('package', 'Install the npm package', `<p>Install <code>@vibefield/rosetta-protocol</code> from npm. It includes ESM JavaScript and TypeScript declarations; no source compilation or repository checkout is required.</p>${table(['Export', 'Use'], [
        ['<code>@vibefield/rosetta-protocol</code>', 'RosettaClient, subscription classes, errors, and generated types. Use a DOM-enabled TypeScript lib.'],
        ['<code>@vibefield/rosetta-protocol/protocol-version</code>', 'Only the wire version, for supervisors without DOM types.'],
        ['<code>@vibefield/rosetta-protocol/grants</code>', 'Node-only grant minting and verification through node:crypto. Keep it out of browser bundles.'],
      ])}<p>Import types through the package root. Deep imports of <code>client</code> or <code>generated/*</code> are not public exports.</p>`)}
      ${s('connect', 'Create a client', `${code('import { RosettaClient } from "@vibefield/rosetta-protocol";\n\nconst client = new RosettaClient({\n  baseUrl: "http://127.0.0.1:4640",\n  requestTimeoutMs: 30_000,\n  notificationIntervalMs: 16,\n});\n\nconst { sessions } = await client.sessions({ limit: 20 });\nconsole.log(sessions);', 'TypeScript')}<p>These are the defaults. For authenticated daemons, supply <code>credential</code>. You can inject <code>fetchFn</code> and <code>WebSocketImpl</code> in other runtimes or tests.</p>`)}
      ${s('query', 'Use the generated request shape', `<p>Wire query parameters can be omitted. In the generated <code>SearchRequest</code> type, absent optional values are represented by explicit <code>null</code> fields; the client omits them from the URL.</p>${code(searchExample, 'TypeScript')}`)}
      ${s('lifecycle', 'Own the consumer’s lifecycle', `${code('const subscription = client.subscribeSession(sessionId, {\n  onChange: entries => console.log(entries),\n  onStatus: status => console.log(status),\n  onError: error => console.error(error.message),\n});\n\n// When the view is disposed:\nsubscription.close();', 'TypeScript')}<p>Call <code>setCredential()</code> when your supervisor refreshes a grant. Call <code>setBaseUrl()</code> if a restarted daemon binds a new origin. Every subscription returns its own <code>close()</code> method.</p><p><a href="${url('subscriptions')}">Read the subscription contract ${icon('chevron')}</a></p>`)}
      ${s('types', 'Types come from Rust', `<p>Canonical row types are not a second client-side model. They are generated with ts-rs from Rust serde shapes. Millisecond timestamps, ordinals, and token counters are JSON numbers; missing facts remain null.</p><p><a href="${url('types')}">Explore the generated type reference ${icon('chevron')}</a></p>`)}
    `,
  },
  {
    id: 'subscriptions', title: 'Live subscriptions', group: 'Build a client', description: 'Follow a conversation through snapshots, mutations, and reconnects without losing consistency.', source: 'crates/server/src/ws.rs',
    render: ({ url }) => `
      ${s('topics', 'Choose a stream', table(['Topic', 'State'], [
        [`<a href="${url('reference/ws-meta')}"><code>/ws/meta</code></a>`, 'Runtimes, projects, and sessions. Fresh metadata snapshot; unstamped changes.'],
        [`<a href="${url('reference/ws-session')}"><code>/ws/sessions/{id}</code></a>`, 'Recent timeline, work index, and stamped mutations. Supports epoch/seq replay.'],
        [`<a href="${url('reference/ws-collaboration')}"><code>/ws/collaboration/{id}</code></a>`, 'Delivered shared conversation and response runtime observations; no private tool rows.'],
      ]))}
      ${s('sdk', 'Subscribe through the SDK', `${code('const subscription = client.subscribeSession(sessionId, {\n  onReset: () => console.log("Timeline generation replaced"),\n  onSnapshot: page => console.log(page.cursor),\n  onChange: entries => console.log(entries),\n  onTimelineMetaChange: meta => console.log(meta.tasks),\n  onStatus: status => console.log(status),\n  onError: error => console.error(error),\n});\n\n// Release the connection when its view closes.\nsubscription.close();', 'TypeScript')}<p>Status callbacks distinguish <code>connecting</code>, <code>syncing</code>, <code>live</code>, <code>resyncing</code>, and <code>closed</code>. Notifications coalesce over 16 ms by default.</p>`)}
      ${s('frames', 'Understand the frame sequence', `<ol class="steps"><li><strong>hello</strong><p>Validate protocol_version. Its stamp advertises the target; it does not acknowledge applied data.</p></li><li><strong>snapshot or replay</strong><p>A new session receives timeline_snapshot. A valid resume replays mutations and completes with caught_up, including when the replay is empty.</p></li><li><strong>mutation</strong><p>Apply at least once, keyed by timeline_id. A mutation can patch an earlier row rather than append to the end.</p></li><li><strong>resync</strong><p>Discard the old generation. A fresh snapshot immediately establishes the replacement baseline.</p></li></ol><p><a href="${url('types/ServerFrame')}">Inspect all ServerFrame variants ${icon('chevron')}</a></p>`)}
      ${s('history', 'Page within the live generation', `${code('const page = await subscription.fetchPage(cursor, 200);\nsubscription.reveal(page.entries);\n\n// Jump to an old task or workflow anchor without walking every page.\nawait subscription.loadEntry(timelineId);', 'TypeScript')}<p>Only call fetchPage with a cursor from the active snapshot or its pages. The subscription retains patches to older rows and invalidates historical reads when the generation changes.</p>`)}
      ${s('reconnect', 'Reconnect from acknowledged state', `<p>Session resumes use <code>?epoch=…&amp;seq=…</code>. Commit the stamp only after the snapshot or complete replay has been applied. Metadata and collaboration streams establish new snapshots instead.</p><p>The SDK backs off from 500 ms to a maximum of 10 seconds, waits for synchronization before resetting its backoff, and has a 15-second handshake deadline. The server sends an idle ping after 20 seconds.</p>${note('This is a server-to-client protocol', 'The endpoint chooses the topic. No subscribe message is required. A raw WebSocket client must handle snapshots, replay, invalidation, and transport keepalive; the SDK already implements those rules.')}`)}
    `,
  },
  daemonLifecycle,
  ingestionLifecycle,
  liveIngestion,
];
