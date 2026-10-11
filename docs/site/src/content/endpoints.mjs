// Editorial descriptions live here; routes, versions, and response types are
// checked against the Rust router and generated TypeScript during every build.
import { searchExample } from './examples.mjs';
const p = (name, type, description, required = false) => ({ name, type, description, required });
const windowQuery = [
  p('since_ms', 'integer', 'Start of the window in Unix milliseconds. Defaults to 0.'),
  p('offset_minutes', 'integer', 'Fixed timezone offset from UTC, in minutes. Defaults to 0; use the same offset across related aggregates.'),
];
const imageQuery = [
  p('image', 'ImageVariant', 'Optional rendition: full, rail, tile, tile2x, or preview. Omit to read the original bytes.'),
  p('v', 'integer', 'Image recipe version. Use 1 when requesting a rendition.'),
  p('retry', 'integer', 'A positive value asks the image cache to retry a failed rendition.'),
];
const sessionPath = '/api/sessions/{id}';
const rowPath = `${sessionPath}/timeline/{timeline_id}`;

export const endpoints = [
  {
    id: 'status', group: 'Daemon', method: 'GET', path: '/api/status', title: 'Get daemon status',
    description: 'Inspect protocol compatibility, canonical publication, search readiness, and live ingestion progress in one read.',
    response: 'StatusResponse', sdk: 'await client.status()',
    notes: ['The listener answers before cold ingestion finishes. A reachable daemon is not necessarily ready for every query.', 'canonical_ready and search.readiness are independent. Wait for canonical_ready before reading project aggregates; inspect search.readiness before treating an empty search as complete.', 'daemon is null when the projection is served without an ingestion loop.'],
    related: ['readiness', 'types/StatusResponse'],
  },
  {
    id: 'discoveries', group: 'Daemon', method: 'GET', path: '/api/discoveries', title: 'List discovered agents',
    description: 'Read the content-free census of agent data roots, including installation, file counts, byte estimates, and enrollment.',
    response: 'DiscoveriesResponse', sdk: 'await client.discoveries()',
    notes: ['Discovery reports inventory without reading transcript content. installed means the root contains at least one file claimed by an adapter.', 'Use enabled to distinguish discovered agents from agents already admitted for ingestion.'],
  },
  {
    id: 'enable-runtimes', group: 'Daemon', method: 'POST', path: '/api/runtimes/enable', title: 'Enable agent runtimes',
    description: 'Grant the initial enrollment set while a fresh daemon is awaiting consent.',
    request: 'EnableRequest', body: { kinds: ['claude-code', 'codex'] }, response: 'EnableResponse',
    sdk: 'await client.enableRuntimes(["claude-code", "codex"])',
    notes: ['Only installed kinds reported by /api/discoveries are accepted. Choose at least one kind. This is a one-time consent decision while enrollment is open.', 'An already-consented daemon returns 400. Restart with the desired --agent, --all-agents, or --enroll configuration to change its set.', 'The browser Origin rule applies to this write in every authentication mode.'],
  },
  {
    id: 'boost', group: 'Daemon', method: 'POST', path: '/api/ingest/boost', title: 'Prioritize a session',
    description: 'Record an advisory request to bring a queued session forward in the backfill scheduler.',
    request: 'BoostRequest', body: { session_id: 'SESSION_ID' }, response: 'BoostResponse', sdk: 'await client.boost(sessionId)',
    notes: ['queued acknowledges the request, not completed ingestion. Without a backfill queue, queued is false.', 'A nonempty id that the queue never encounters simply expires. It is safe to request a boost when a reader selects a pending session.'],
  },
  {
    id: 'runtimes', group: 'Daemon', method: 'GET', path: '/api/runtimes', title: 'List runtimes',
    description: 'List canonical runtimes, including the presentation metadata of supervisor-enrolled roots.',
    response: 'RuntimesResponse', sdk: 'await client.runtimes()',
    notes: ['Rows are sorted by canonical runtime id. A family such as claude-code can have several enrolled runtimes; identify a row by id, not kind.'],
  },
  {
    id: 'projects', group: 'Projects', method: 'GET', path: '/api/projects', title: 'List projects',
    description: 'List the local checkout projects established during bounded metadata discovery.',
    response: 'ProjectsResponse', sdk: 'await client.projects()',
    notes: ['Sorted by name, then id. Linked Git worktrees share a project; ordinary clones remain distinct.', 'A session can have no proven project. Do not infer project ownership from later tool paths or a shared remote URL.'],
  },
  {
    id: 'heatmap', group: 'Projects', method: 'GET', path: '/api/projects/{id}/heatmap', title: 'Get project activity',
    description: 'Read daily activity and usage for a project over a fixed timezone window.',
    query: windowQuery, response: 'HeatmapResponse', sdk: 'await client.heatmap(projectId, { offsetMinutes: 0 })', canonical: true,
    notes: ['Use the reserved project id __rosetta_unassigned__ for sessions without a project. Usage and heatmap queries share the same window and timezone rules.'],
  },
  {
    id: 'usage', group: 'Projects', method: 'GET', path: '/api/projects/{id}/usage', title: 'Get usage breakdown',
    description: 'Decompose a project’s usage into model-by-day totals and an hour-of-day profile.',
    query: windowQuery, response: 'UsageResponse', sdk: 'await client.usage(projectId, { offsetMinutes: 0 })', canonical: true,
    notes: ['Use the same since_ms and offset_minutes as the heatmap so the totals reconcile. __rosetta_unassigned__ selects sessions with no project.'],
  },
  {
    id: 'day-usage', group: 'Projects', method: 'GET', path: '/api/projects/{id}/usage/day', title: 'Get one day’s usage',
    description: 'Inspect hourly per-model usage for one calendar day in a specified timezone offset.',
    query: [p('day', 'string', 'Calendar date in YYYY-MM-DD form. Impossible or noncanonical dates are rejected.', true), p('offset_minutes', 'integer', 'UTC offset in minutes, from −840 to 840. Defaults to 0.')],
    exampleQuery: '?day=2026-10-05&offset_minutes=0', response: 'DayUsageResponse', sdk: 'await client.dayUsage(projectId, "2026-10-05", 0)', canonical: true,
    notes: ['Use __rosetta_unassigned__ to inspect sessions with no project. The day and offset are validated before the read.'],
  },
  {
    id: 'contribution', group: 'Projects', method: 'GET', path: '/api/projects/{id}/contribution', title: 'Get contributions',
    description: 'Read the project’s per-session contribution rollup from the canonical projection.',
    response: 'ContributionResponse', sdk: 'await client.contribution(projectId)', canonical: true,
    notes: ['Use __rosetta_unassigned__ for the unassigned project scope. The rollup describes recorded history, not live agent execution.'],
  },
  {
    id: 'pricing', group: 'Projects', method: 'GET', path: '/api/pricing', title: 'Inspect pricing coverage',
    description: 'Inspect the compiled pricing catalog, the projection’s pricing version, source provenance, and unpriced model usage.',
    response: 'PricingResponse', sdk: 'await client.pricing()',
    notes: ['basis is list_price_equivalent: the model creator’s API list-price equivalent for recorded usage. Provider-reported cost is a separate fact.', 'unpriced reports unknown models and usage the catalog could not price. Missing coverage must not be presented as a zero-cost result.'],
  },
  {
    id: 'activity', group: 'Sessions', method: 'GET', path: '/api/activity', title: 'Get session activity',
    description: 'Read top-level sessions with recorded activity in a time window, including the turns and gaps in each session tree.',
    source: 'crates/server/src/http/activity.rs',
    query: [p('since_ms', 'integer', 'Inclusive window start in Unix milliseconds.', true), p('until_ms', 'integer', 'Inclusive window end in Unix milliseconds. Defaults to the current time; must follow since_ms by at most seven days.'), p('sessions', 'string', 'Up to 256 comma-separated canonical top-level session ids. Values are trimmed and deduplicated; blank entries are rejected. Child ids do not select a root.'), p('gap_ms', 'integer', 'Report silences strictly longer than this threshold. Defaults to 900,000 (15 minutes); minimum 60,000 (one minute).'), p('limit', 'integer', 'Maximum sessions. Defaults to 200; clamped to 1–1,000.')],
    exampleQuery: '?since_ms=1791158400000&until_ms=1791244800000', response: 'ActivityResponse',
    sdk: 'await client.activity({\n  sinceMs: 1791158400000,\n  untilMs: 1791244800000,\n})',
    notes: ['Sessions are ordered by newest recorded activity. Each session includes its descendant tree’s turns, oldest first. Activity bounds select sessions, so a selected session can have no turns touching the window.', 'Turn bounds come from rows overlapping the window and are not clipped to it. Gaps measure the absence of recorded activity, including long waits inside a tool call; they are not proof that an agent stopped running.', 'truncated means additional matching sessions were omitted by limit. This is a bounded response without a pagination cursor.', 'During cold admission, a pending session can have turns_withheld: true and an empty turns array. This does not mean the session has no history.', 'In authenticated mode this cross-session endpoint requires the bearer; a session grant cannot open it. Invalid window bounds, gap thresholds, or session selectors return 400.'],
    related: ['reference/sessions', 'readiness', 'types/TurnSpan'],
  },
  {
    id: 'sessions', group: 'Sessions', method: 'GET', path: '/api/sessions', title: 'List sessions',
    description: 'Browse root conversations in recorded activity order, or resolve sessions from provider-native identifiers.',
    query: [p('project', 'string', 'Filter by canonical project id.'), p('limit', 'integer', 'Maximum rows. Defaults to 100; clamped to 1–1,000.'), p('native_session_id', 'string', 'Comma-separated provider-native ids. Values are trimmed and deduplicated; blank entries are rejected. This lookup includes child sessions.')],
    exampleQuery: '?limit=20', response: 'SessionsResponse', sdk: 'await client.sessions({ limit: 20 })',
    notes: ['Ordinary browsing returns sessions without a parent_session_id. A native-id lookup can return children; use root_session_id to navigate to the root conversation.', 'activity describes timestamped conversation evidence; updated_at is metadata freshness. stream_state describes indexing, not whether the agent is running.', 'This is a bounded list, not a cursor-paginated collection. Use the timeline endpoint to page through a session’s history.'],
  },
  {
    id: 'session', group: 'Sessions', method: 'GET', path: sessionPath, title: 'Get a session',
    description: 'Resolve one canonical session, including a child session, by its id.',
    response: 'SessionResponse', sdk: 'await client.session(sessionId)', grant: 'read',
    notes: ['Returns 404 when the metadata mirror has no matching session. IDs are opaque; encode each path segment.'],
  },
  {
    id: 'timeline', group: 'Sessions', method: 'GET', path: `${sessionPath}/timeline`, title: 'Read a timeline',
    description: 'Read a backward page of the canonical conversation. Entries within each page are ordered oldest first.',
    query: [p('limit', 'integer', 'Page size. Defaults to 100; clamped to 1–1,000.'), p('cursor', 'string', 'Opaque continuation from the previous response. Omit to read the latest page.')],
    response: 'TimelinePage', sdk: 'await client.timeline(sessionId, { limit: 100 })', grant: 'read',
    notes: ['The first page binds a session watermark. Continue using cursor until it is null; never construct a cursor yourself.', 'A rewritten projection or invalid cursor returns 410 cursor_expired. Discard cached pages and refetch from the head.', 'While a cold runtime is pending admission, a known session can return an empty page. That is not proof that the conversation is empty.'], related: ['pagination', 'subscriptions'],
  },
  {
    id: 'timeline-entry', group: 'Sessions', method: 'GET', path: rowPath, title: 'Get a timeline entry',
    description: 'Read an exact transcript anchor and the projection watermark it belongs to, without paging through intervening history.',
    response: 'TimelineEntryResponse', sdk: 'await client.timelineEntryResponse(sessionId, timelineId)', grant: 'read',
    notes: ['timelineEntry() is the SDK convenience method that unwraps entry. When working inside a subscription, use subscription.loadEntry() to preserve generation consistency.'],
  },
  {
    id: 'collaboration', group: 'Sessions', method: 'GET', path: `${sessionPath}/collaboration`, title: 'Read shared conversation',
    description: 'Read a team member’s complete delivered collaboration history and its response runtime observations.',
    response: 'CollaborationTimeline', sdk: 'await client.collaborationTimeline(sessionId)', grant: 'read',
    notes: ['This projection excludes private prose and tool rows. It is distinct from the member’s full timeline.', 'Use /ws/collaboration/{id} for a live shared-conversation view.'],
  },
  {
    id: 'command', group: 'Content & assets', method: 'GET', path: `${rowPath}/command`, title: 'Read a command body',
    description: 'Recover the exact shell or code source deliberately omitted from a timeline row, lazily from the raw ledger.',
    response: 'CommandResponse', sdk: 'await client.command(sessionId, timelineId)', grant: 'read',
    notes: ['Fetch on explicit disclosure. Timeline pages intentionally do not carry these potentially large command bodies.'],
  },
  {
    id: 'work', group: 'Content & assets', method: 'GET', path: `${rowPath}/work`, title: 'Read reported work',
    description: 'Expand the private work behind an exact report anchor into its canonical timeline rows.',
    response: 'TimelineEntry', array: true, sdk: 'await client.reportWork(sessionId, timelineId)', grant: 'read',
    notes: ['Use the session and timeline ids from the recorded report. The response is an array of entries, not a TimelinePage.'],
  },
  {
    id: 'file', group: 'Content & assets', method: 'GET', path: `${rowPath}/read/{target_index}`, title: 'Read a recorded file target',
    description: 'Read the current file on disk named by a canonical file-bearing target. The request supplies identities and an index, never a filesystem path.',
    query: imageQuery, response: 'binary', sdk: 'await client.readFileTarget(sessionId, timelineId, 0)', grant: 'read + files',
    notes: ['The bytes are what the file contains now, not a historical snapshot. Raw reads use application/octet-stream, Cache-Control: no-store, and a 32 MiB ceiling.', 'A grant needs both read and files, and an exact session match. The browser Origin guard also applies.', 'Use imageFileTargetUrl() for a rendition URL. URL helpers do not attach Authorization; authenticated consumers must supply credentials when fetching the bytes.'],
  },
  {
    id: 'plan-revision', group: 'Content & assets', method: 'GET', path: `${sessionPath}/plans/revisions/{revision}`, title: 'Read a plan revision',
    description: 'Resolve a captured plan document by its session-scoped revision number.',
    response: 'document', sdk: 'await client.readPlan(sessionId, { type: "revision", revision: 1 })', grant: 'read',
    notes: ['Returns the document bytes as text/markdown or application/json, depending on the recorded name, with private, no-store caching.', 'Unavailable or not-yet-admitted revisions return 404. Prefer the source reference supplied by TimelineMeta.'],
  },
  {
    id: 'timeline-plan', group: 'Content & assets', method: 'GET', path: `${rowPath}/plan`, title: 'Read a recorded plan',
    description: 'Resolve the plan document named by an exact timeline entry.',
    response: 'document', sdk: 'await client.readPlan(sessionId, { type: "timeline", timeline_id: timelineId })', grant: 'read',
    notes: ['Returns text/markdown or application/json with private, no-store caching. Use the exact source reference from TimelineMeta.'],
  },
  {
    id: 'asset', group: 'Content & assets', method: 'GET', path: '/api/assets/{name}', title: 'Read a projection asset',
    description: 'Read immutable, content-addressed media or oversized canonical text referenced by a transcript.',
    query: imageQuery, response: 'binary', sdk: 'await client.readAsset(asset)', grant: 'read',
    notes: ['name is a 64-character lowercase BLAKE3 digest plus .png, .jpg, .gif, .webp, .pdf, .json, or .txt. Follow the stored AssetRef.url instead of constructing a name.', 'Original members are checked against their digest and returned with private, max-age=31536000, immutable caching. Image renditions support conditional requests.', 'A valid read grant can open a known asset digest without a session comparison. For images use imageAssetUrl(); readTextAsset() is for canonical oversized text.'],
  },
  {
    id: 'search', group: 'Search', method: 'GET', path: '/api/search', title: 'Search agent history',
    description: 'Search recorded user prompts and agent replies across runtimes using a backend-neutral query and structured filters.',
    query: [p('q', 'string', 'Search text; double quotes preserve a phrase. No backend-specific FTS syntax.', true), p('limit', 'integer', 'Maximum hits. Defaults to 50; clamped to 1–1,000.'), p('session', 'string', 'One canonical session. Cannot be combined with sessions.'), p('sessions', 'string', 'Up to 256 comma-separated canonical session ids; each widens to its descendant tree. Blank entries are rejected.'), p('prefix', 'boolean', 'Defaults to false. Expands unquoted terms of at least three characters to word prefixes; phrases stay exact.'), p('project', 'string', 'Canonical project id.'), p('runtime', 'string', 'One concrete runtime id.'), p('agent', 'string', 'Agent family, for example claude-code or codex.'), p('kind', 'string', 'user or agent. Other kinds are rejected with 400.'), p('is_error', 'boolean', 'Currently rejected: tool activity is not indexed.'), p('after_ts', 'integer', 'Lower timestamp bound in Unix milliseconds.'), p('before_ts', 'integer', 'Upper timestamp bound in Unix milliseconds.')],
    exampleQuery: '?q=authentication&limit=20', response: 'SearchResponse', sdk: searchExample,
    notes: ['Every response includes state. While canonical publication is pending, this endpoint returns 200 with building readiness and no hits.', 'q accepts at most 4,096 bytes and 64 required terms or phrases. Selectors are nonblank canonical values, at most 1,024 bytes; after_ts must not exceed before_ts. Explicit time bounds exclude rows with unknown timestamps.', 'Order is score, then stable timeline_id. Scores are diagnostic; do not compare them across backends or index generations.', 'Check readiness, projection_epoch, and revision before declaring that a search is complete.'], related: ['readiness', 'types/SearchHit'],
  },
  {
    id: 'ws-meta', group: 'Live streams', method: 'WS', path: '/ws/meta', title: 'Subscribe to metadata',
    description: 'Observe the live runtime, project, and session catalog from the daemon’s resident metadata mirror.',
    response: 'ServerFrame', sdk: 'client.subscribeMeta({\n  onChange: (state, source) => {\n    console.log(source, state.sessions);\n  },\n})',
    notes: ['Starts with hello and meta_snapshot, then unstamped mutations. Reconnect establishes a fresh baseline.', 'The SDK distinguishes snapshot and mutation callbacks so a reconnect does not manufacture new activity. Close the returned subscription when the consumer unmounts.'],
  },
  {
    id: 'ws-session', group: 'Live streams', method: 'WS', path: '/ws/sessions/{id}', title: 'Subscribe to a timeline',
    description: 'Stream a session’s recent transcript window, work index, and subsequent canonical mutations.',
    query: [p('epoch', 'unsigned integer', 'Last acknowledged epoch. Use together with seq to request replay.'), p('seq', 'unsigned integer', 'Last acknowledged sequence. Commit a resume stamp only after an applied snapshot or caught_up.')],
    response: 'ServerFrame', sdk: 'client.subscribeSession(sessionId, {\n  onChange: (entries) => console.log(entries),\n  onStatus: (status) => console.log(status),\n})', grant: 'read',
    notes: ['The initial timeline_snapshot contains up to 200 entries, oldest first, plus timeline_meta, watermark, cursor, and stamp.', 'Mutations are delivered at least once. Key rows by timeline_id; patches can change rows already loaded in older pages.', 'resync is a barrier: discard the old generation and apply the replacement snapshot. hello advertises a position but never acknowledges received data.', 'Older history uses the REST timeline endpoint. The SDK coordinates pages, exact reads, replay, and invalidation.'], related: ['subscriptions', 'pagination'],
  },
  {
    id: 'ws-collaboration', group: 'Live streams', method: 'WS', path: '/ws/collaboration/{id}', title: 'Subscribe to shared conversation',
    description: 'Stream one team member’s delivered collaboration messages without the private work behind them.',
    response: 'ServerFrame', sdk: 'client.subscribeCollaboration(sessionId, {\n  onChange: (state) => console.log(state.entries),\n})', grant: 'read',
    notes: ['Starts with hello and collaboration_snapshot; subsequent frames include collaboration mutations and response_runtime observations.', 'This stream is unstamped and replaces its snapshot on resync. It does not use session replay cursors.'],
  },
];
