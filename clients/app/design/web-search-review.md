# Web activity implementation review — 2026-09-09

Compared the production transcript with `transcript-record.html`, §04 “The web lines, drawn,” §06 absence/failure states, and Q6 site icons. Rendered both the design specimen and the real React components in Electron.

| Requirement | Finding and completed change |
| --- | --- |
| Search query, source count, three-site icon stack | The drawing existed, but Claude's structured results never populated it. The Claude adapter now correlates the request with `toolUseResult.results[].content[]`, retains titled addresses, and preserves the total beyond the canonical ten-source preview. |
| Titled, clickable source rows | Only titles were links. The whole row now opens the recorded HTTP(S) destination, supports keyboard focus, and exposes its full title/address on hover. Unusable addresses stay plain text. |
| Colored 14px icons, rounded corners, globe fallback | Existing styling and main-process per-host cache match the design. Added image-decode fallback and protected the renderer cache against stale requests when the setting changes. No third-party icon service was introduced. |
| Named “Fetch site icons” preference | The preference existed without a control. Every web detail now exposes the shared, persistent checkbox. Disabling it replaces icons with globes and prevents new requests; reenabling correctly requests active hosts, including a batched off/on transition. |
| Source details survive folds | Fold totals incorrectly became the number of retained rows. They now preserve the reported total, distinguish partial information, and let the reader reveal all retained sources beyond the initial 24-row display. |
| Fetch/open detail: final URL, title, HTTP status | Folded fetch details were discarded, and URLs without an outcome lost their scheme. Details now keep every recorded page, use usable links, and retain redirects/status. Find-in-page also retains its searched pattern. |
| Failure and absence | Recorded HTTP failures retain their status and diagnostic. A proven zero differs from unrecorded results. Missing titles and sources are not fabricated. |
| Readability in a narrow message view | Long domains and page titles crowded out the main target. Each web entry now queries its own width, moving facts or domains below their associated text when necessary. The wider source list retains the design's 26px rows. |

## Native-data evidence and limits

Claude's pinned [observed tool facets](../../../groundtruth/claude-code/sources/observed-2026-09-03.json) define WebSearch results as narrative strings or groups of `{title, url}` records. Only those structured source records are projected. WebFetch persists `url` and HTTP `code`; its `result` is extracted prose and `codeText` is a status phrase, so neither becomes a page title. Correlation state survives a checkpoint between request and result.

Codex's pinned [native `WebSearchAction` and response item](https://github.com/openai/codex/blob/rust-v0.150.1/codex-rs/protocol/src/models.rs) retain search actions and `query`/`queries`, but no result source list. The adapter now retains batched `queries`; those records still correctly display “No source details recorded.” Unstructured tool prose is not treated as a source manifest.

Claude normalizer version 25 and Codex version 13 invalidate prior checkpoints. After restarting with the updated daemon, the existing ingest pipeline replays stale projections, including quiet source files. No manual database edit is needed. The app's `npm run dev` builds that daemon before launch.

## Verification

- Production renderer build and TypeScript check: passed.
- 89 focused transcript/web/favicon tests: passed, including folded totals, redirects, omitted results, and find-in-page targets.
- 166 tests across the full Claude and Codex adapter suites: passed, including checkpoint continuation, bounded Unicode titles, source preview limits, and native batched queries.
- `npm run test:web-search`: isolated Electron profile, real production components, deterministic icon responses, no external requests or archive reads. Checks whole-row links, per-host caching, missing/invalid icons, preference persistence/races, all retained sources, folded pages, keyboard disclosure, and clipping/readable target widths at 980px, 660px, and 440px in light/dark themes.

The design HTML remains the reference. The additional responsive rows and setting control make its specified behavior usable at production widths.

The final Rust verification ran against committed HEAD plus this web patch in `/tmp/rosetta-web-verify-hbc_5h84`. A concurrent attachment-model migration changed `AttachmentRef` in the shared workspace before its consumers were migrated, causing unrelated compilation errors. The isolated verification excludes that unfinished migration; the shared daemon was not rebuilt or restarted.
