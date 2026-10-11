# Rosetta API documentation

A static documentation website built from Rosetta's current service contract.
Node 22 or newer is the only requirement. No dependency installation, daemon,
API credentials, external fonts, or third-party network calls are needed.

From the repository root:

```sh
npm --prefix docs/site run dev
# http://127.0.0.1:5190 — rebuilds and reloads when source changes

npm --prefix docs/site run check
npm --prefix docs/site run build
npm --prefix docs/site run preview
```

Set `PORT=5191` if the default port is occupied. The preview binds to loopback.

## What is documented

- Onboarding, architecture, authentication and grants, readiness, pagination,
  compatibility, the TypeScript client, and live subscriptions.
- Every route registered in `crates/server/src/http.rs`, with request examples,
  parameters, authentication scope, edge cases, and linked response types.
- All checked-in generated types, their field documentation, dependencies,
  dependents, and corresponding endpoints.
- Separately labeled cold and live ingestion lifecycle proposals, summarized from
  [`../INGESTION_API_PROPOSAL.md`](../INGESTION_API_PROPOSAL.md). Proposed routes
  are excluded from the implemented endpoint inventory and generated type reference.
- Desktop and Vibefield integration requirements from the
  [implementation review](../INGESTION_API_REVIEW.md), including enrollment,
  host-owned controls, archive retention, and generation-aware cache invalidation.
- A separately labeled [daemon lifecycle design](../DAEMON_LIFECYCLE_PROPOSAL.md):
  process ownership, private shutdown, health, retry limits, and platform release gates.

Search runs locally in the browser, including full page contents. `⌘K` / `Ctrl+K`
opens it; `/` focuses a reference filter or opens search. All document navigation
and content also work without JavaScript. Theme and mobile navigation use a small
progressive enhancement layer; no live API requests are made by the website.

## Keeping the reference accurate

The router and generated contract are authoritative. Every build checks:

- Exact HTTP/WebSocket route coverage, including additions and removals.
- Matching Rust and TypeScript wire versions.
- Existing request/response types, SDK methods, and type dependencies.
- Unique pages and HTML ids; every internal link and fragment.

Editorial route metadata lives in `src/content/endpoints.mjs`; narrative pages in
`src/content/guides.mjs`. Update both when changing semantics. Parameter defaults,
authentication scopes, and examples still need review against their handlers;
coverage checks do not prove every editorial statement.

The build reads the checked-in `clients/ts/generated` files; it does not regenerate
them. Regenerate the contract through the Rust protocol workflow first when the
wire shape changes. The documentation check runs in the existing TypeScript CI job.

## Design provenance

The website consumes `clients/app/renderer/src/styles/tokens.css` at build time,
the app's checked-in transcription of Vibefield's design kit. The readable state
inks in `style.css` follow `vibe-field/DESIGN.md` §2.5. `src/glyphs.mjs` is an unchanged
subset of the official Field Glyph drawings (§13), captured 2026-10-05.

The document layout uses the same system fonts, neutral surfaces, opacity hierarchy,
hairlines, control radii, and dual-theme materials. The code examples use the kit's
committed dark card surface. No sibling checkout is required to build the site.

## Static hosting

`dist/` is the complete output. Serve it with any static host that resolves
directory URLs to `index.html`. Links and assets are relative, so the output also
works below a path prefix such as `/rosetta/`. `.nojekyll` is included for hosts
that recognize it. Deploy the directory atomically so readers receive matching
pages, assets, and search index. No deployment or access policy is configured here.

The source links target the private `vibefield/rosetta` repository and require
repository access. The generated website itself contains API documentation only;
it never includes transcript data, local file contents, or credentials.
