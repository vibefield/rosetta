import { escape as e, icon, code, tabs, table, note, section, sourceUrl } from './html.mjs';

export function renderEndpoint(endpoint, ctx) {
  const { url, types } = ctx;
  const pathParams = [...endpoint.path.matchAll(/\{(\w+)\}/g)].map(([, name]) => [
    `<code>${name}</code><small class="required">required</small>`,
    ['revision', 'target_index'].includes(name) ? 'integer' : 'string',
    ({ id: endpoint.group === 'Projects' ? 'Canonical project id, or __rosetta_unassigned__.' : 'Canonical session id; URL-encode the path segment.', timeline_id: 'Exact timeline_id of the recorded entry; URL-encode the path segment.', target_index: 'Zero-based index of a canonical file-bearing target.', revision: 'Session-scoped plan revision.', name: 'Content-addressed asset name from the stored URL.' })[name],
  ]);
  const examplePath = endpoint.path.replace('{id}', endpoint.group === 'Projects' ? 'PROJECT_ID' : 'SESSION_ID').replace('{timeline_id}', 'TIMELINE_ID').replace('{target_index}', '0').replace('{revision}', '1').replace('{name}', 'ASSET_NAME');
  let curl = `curl ${endpoint.method === 'POST' ? '-X POST ' : ''}"http://127.0.0.1:4640${examplePath}${endpoint.exampleQuery ?? ''}"`;
  if (endpoint.body) curl += ` \\\n  -H "Content-Type: application/json" \\\n  -d '${JSON.stringify(endpoint.body)}'`;
  const sdk = endpoint.sdk.endsWith(';') ? endpoint.sdk : endpoint.sdk + ';';
  const rawSocket = `const socket = new WebSocket(\n  "ws://127.0.0.1:4640${examplePath}",\n);\n\nsocket.addEventListener("message", event => {\n  console.log(JSON.parse(event.data));\n});`;
  return `<div class="endpoint-address"><span class="method method-${endpoint.method.toLowerCase()}">${endpoint.method}</span><code>${e(endpoint.path)}</code></div>
    <div class="endpoint-facts"><span>${icon('key')}${endpoint.grant ? `Bearer or grant · ${e(endpoint.grant)}` : 'Bearer when configured'}</span><span>${endpoint.method === 'WS' ? 'Server → client' : endpoint.response === 'binary' ? 'Binary response' : endpoint.response === 'document' ? 'Document response' : 'JSON response'}</span></div>
    ${section('request', 'Request', tabs(endpoint.method === 'WS' ? [['TypeScript SDK', sdk], ['WebSocket', rawSocket]] : [['cURL', curl], ['TypeScript', sdk]], 'request', { dark: true }) + `<p class="small">Examples use an open local daemon.${endpoint.method === 'WS' ? ' Close the subscription when its consumer is disposed.' : ''} For configured access, <a href="${url('authentication')}">present a bearer or eligible grant</a>. TypeScript examples assume a <a href="${url('typescript')}">RosettaClient instance</a> and ids from earlier responses.</p>`)}
    ${pathParams.length ? section('path-parameters', 'Path parameters', table(['Name', 'Type', 'Description'], pathParams)) : ''}
    ${endpoint.query?.length ? section('query-parameters', 'Query parameters', table(['Name', 'Type', 'Description'], endpoint.query.map(param => [`<code>${e(param.name)}</code>${param.required ? '<small class="required">required</small>' : ''}`, types.has(param.type) ? `<a href="${url(`types/${param.type}`)}">${param.type}</a>` : e(param.type), e(param.description)]))) : ''}
    ${endpoint.request ? section('request-body', 'Request body', `<p>Send <code>application/json</code> matching <a href="${url(`types/${endpoint.request}`)}">${endpoint.request}</a>.</p>${code(ctx.formattedType(endpoint.request), 'TypeScript', { types: new Set(types.keys()), url })}`) : ''}
    ${section('response', endpoint.method === 'WS' ? 'Server frames' : 'Response', types.has(endpoint.response)
      ? `<div class="response-heading"><span class="response-status">${endpoint.method === 'WS' ? '101 Switching Protocols' : '200 OK'}</span><a href="${url(`types/${endpoint.response}`)}">${endpoint.response}${endpoint.array ? '[]' : ''} ${icon('chevron')}</a></div>${code(endpoint.array ? `// Response body: TimelineEntry[]\n${ctx.formattedType(endpoint.response)}` : ctx.formattedType(endpoint.response), 'Generated contract', { types: new Set(types.keys()), url })}`
      : `<p>${endpoint.response === 'binary' ? 'The response contains bytes. Its content type and caching behavior depend on the resource and optional image rendition.' : 'Returns the recorded document bytes as text/markdown or application/json, with private, no-store caching.'}</p>`)}
    ${section('behavior', 'Behavior & edge cases', `${endpoint.canonical ? note('Wait for canonical publication', `Before publication this endpoint returns 503 <code>not_ready</code>. Check <a href="${url('reference/status')}">canonical_ready</a> before fetching aggregates.`) : ''}<ul class="behavior-list">${endpoint.notes.map(text => `<li>${e(text)}</li>`).join('')}</ul><p>See <a href="${url('errors')}">errors and compatibility</a> for shared response handling.</p>`)}
    ${endpoint.related?.length ? section('related', 'Keep reading', `<div class="related-links">${endpoint.related.map(id => `<a href="${url(id)}">${e(ctx.pageTitle(id))}${icon('chevron')}</a>`).join('')}</div>`) : ''}`;
}

export function renderReference(ctx) {
  const groups = Map.groupBy(ctx.endpoints, item => item.group);
  return `<label class="filter-field">${icon('search')}<input type="search" data-filter placeholder="Filter by route, name, or method…" aria-label="Filter endpoints"><kbd>/</kbd></label><p class="filter-count" aria-live="polite" data-filter-count>${ctx.endpoints.length} endpoints</p>
    ${[...groups].map(([group, endpoints]) => `<section class="doc-section filter-group" data-filter-group><h2 id="${group.toLowerCase().replace(/[^a-z]+/g, '-')}">${e(group)}</h2><div class="endpoint-list">${endpoints.map(endpoint => `<a class="endpoint-row" data-filter-item data-search="${e(`${endpoint.title} ${endpoint.path} ${endpoint.method} ${group}`.toLowerCase())}" href="${ctx.url(`reference/${endpoint.id}`)}"><span class="method method-${endpoint.method.toLowerCase()}">${endpoint.method}</span><div><strong>${e(endpoint.title)}</strong><code>${e(endpoint.path)}</code></div>${icon('chevron')}</a>`).join('')}</div></section>`).join('')}
    <div class="filter-empty" data-filter-empty hidden>No endpoints match. Try a shorter route or a method such as GET.</div>`;
}

export function renderTypes(ctx) {
  const popular = ['SessionRow', 'TimelineEntry', 'TimelineBody', 'ServerFrame', 'SearchRequest', 'SearchResponse', 'StatusResponse', 'TimelineMeta'];
  return `${section('core-types', 'Start with the core types', `<div class="type-picks">${popular.map(name => `<a href="${ctx.url(`types/${name}`)}">${icon('code')}<span>${name}</span>${icon('chevron')}</a>`).join('')}</div>`)}
    ${section('all-types', 'All generated types', `<p>The definitions below come directly from <code>clients/ts/generated</code>, including the Rust field documentation and nullable wire shapes.</p><label class="filter-field">${icon('search')}<input type="search" data-filter placeholder="Find a type…" aria-label="Filter types"><kbd>/</kbd></label><p class="filter-count" aria-live="polite" data-filter-count>${ctx.types.size} types</p><div class="type-index">${[...ctx.types].map(([name, info]) => `<a href="${ctx.url(`types/${name}`)}" data-filter-item data-search="${e(`${name} ${info.summary}`.toLowerCase())}"><code>${name}</code>${icon('chevron')}</a>`).join('')}</div><div class="filter-empty" data-filter-empty hidden>No types match. Try “Session” or “Timeline”.</div>`)}
  `;
}

export function renderType(name, ctx) {
  const info = ctx.types.get(name);
  const usedBy = [...ctx.types].filter(([, other]) => other.dependencies.includes(name)).map(([key]) => key);
  const endpoints = ctx.endpoints.filter(ep => ep.response === name || ep.request === name);
  return `<div class="type-provenance">${icon('code')} Generated by ts-rs <span>·</span> Rust serde is the wire contract</div>
    ${section('definition', 'Definition', code(ctx.formattedType(name), 'TypeScript', { types: new Set([...ctx.types.keys()].filter(key => key !== name)), url: ctx.url }))}
    ${ctx.publicTypes.has(name) ? section('import', 'Import', code(`import type { ${name} } from "@vibefield/rosetta-protocol";`, 'TypeScript')) : note('Referenced definition', 'This supporting type is referenced by generated wire types. Consume the public contract through the package root.')}
    ${info.dependencies.length ? section('references', 'Referenced types', `<div class="type-pills">${info.dependencies.map(type => `<a href="${ctx.url(`types/${type}`)}">${type}</a>`).join('')}</div>`) : ''}
    ${endpoints.length ? section('endpoints', 'Used by endpoints', `<div class="related-links">${endpoints.map(ep => `<a href="${ctx.url(`reference/${ep.id}`)}"><span class="method">${ep.method}</span>${e(ep.title)}${icon('chevron')}</a>`).join('')}</div>`) : ''}
    ${usedBy.length ? section('used-by', 'Referenced by', `<div class="type-pills">${usedBy.map(type => `<a href="${ctx.url(`types/${type}`)}">${type}</a>`).join('')}</div>`) : ''}`;
}

function sidebar(page, ctx) {
  const guides = Map.groupBy(ctx.guides, item => item.group);
  const groups = Map.groupBy(ctx.endpoints, item => item.group);
  const navLink = (id, title, extra = '') => `<a href="${ctx.url(id)}" ${page.id === id ? 'aria-current="page"' : ''}>${extra}<span>${e(title)}</span></a>`;
  return `<button class="search-trigger" type="button" data-search-open>${icon('search')}<span>Search documentation</span><kbd>⌘ K</kbd></button><nav aria-label="Documentation">
    ${[...guides].map(([group, items]) => `<div class="nav-group"><h2>${group}</h2>${items.map(item => navLink(item.id, item.title === 'How Rosetta fits together' ? 'Architecture' : item.title)).join('')}</div>`).join('')}
    <div class="nav-group"><h2>API reference</h2>${navLink('reference', 'All endpoints')}${[...groups].map(([group, endpoints]) => `<details class="nav-disclosure" ${endpoints.some(item => page.id === `reference/${item.id}`) ? 'open' : ''}><summary>${e(group)}<span>${endpoints.length}</span>${icon('chevron')}</summary><div>${endpoints.map(item => navLink(`reference/${item.id}`, item.title, `<span class="nav-method">${item.method}</span>`)).join('')}</div></details>`).join('')}</div>
    <div class="nav-group"><h2>Contract</h2>${navLink('types', 'Type reference', icon('code'))}</div></nav><div class="sidebar-foot"><span>Part of <strong>Vibefield</strong></span><span>Protocol ${ctx.version}</span></div>`;
}

export function renderPage(page, content, ctx) {
  const headings = [...content.matchAll(/<h2 id="([^"]+)"[^>]*>([\s\S]*?)<\/h2>/g)].map(([, id, text]) => ({ id, text: text.replace(/<span[\s\S]*?<\/span>/g, '').replace(/<[^>]+>/g, '') }));
  const group = page.group ?? 'Reference';
  const isOverview = page.id === '';
  const active = page.id.startsWith('types') ? 'types' : page.id.startsWith('reference') ? 'reference' : 'guides';
  const description = e(page.description);
  const selectedIndex = ctx.guides.findIndex(guide => guide.id === page.id);
  const next = selectedIndex >= 0 ? ctx.guides[selectedIndex + 1] : undefined;
  return `<!doctype html>
<html lang="en" data-page="${e(page.id)}"><head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><meta name="color-scheme" content="light dark">
<title>${isOverview ? 'Rosetta — Developer documentation' : `${e(page.title)} — Rosetta docs`}</title><meta name="description" content="${description}"><meta name="theme-color" content="#fafafa">
<meta property="og:title" content="${e(page.title)} — Rosetta docs"><meta property="og:description" content="${description}"><meta property="og:type" content="website">
<link rel="icon" href="${ctx.asset('favicon.svg')}" type="image/svg+xml"><link rel="stylesheet" href="${ctx.asset('tokens.css')}"><link rel="stylesheet" href="${ctx.asset('style.css')}">
<script src="${ctx.asset('theme.js')}"></script><script type="module" src="${ctx.asset('app.js')}"></script>
<noscript><style>.search-trigger,.theme-toggle,.mobile-search,.menu-toggle,.copy-button{display:none}.code-tabs [role="tabpanel"][hidden]{display:block!important}@media(max-width:800px){.sidebar{position:static;transform:none;height:auto;width:auto;margin:20px;border:1px solid var(--vf-hairline-strong);border-radius:12px}.sidebar nav{display:grid;grid-template-columns:1fr 1fr;gap:12px}.mobile-bar{position:static}}</style></noscript>
</head><body>
<a class="skip-link" href="#main">Skip to content</a>
<header class="site-header"><a class="brand" href="${ctx.url('')}" aria-label="Rosetta documentation home"><span class="brand-mark">${icon('layers')}</span><span>rosetta</span><span class="brand-divider"></span><span class="brand-docs">Docs</span></a>
<nav class="top-nav" aria-label="Sections"><a href="${ctx.url('')}" ${active === 'guides' ? 'aria-current="true"' : ''}>Guides</a><a href="${ctx.url('reference')}" ${active === 'reference' ? 'aria-current="true"' : ''}>API reference</a><a href="${ctx.url('types')}" ${active === 'types' ? 'aria-current="true"' : ''}>Types</a></nav>
<div class="header-actions"><span class="version-tag">v${ctx.version}<span> protocol</span></span><a class="github-link" href="https://github.com/vibefield/rosetta">GitHub ${icon('outward')}</a><span class="header-rule"></span><button class="icon-button theme-toggle" data-theme-toggle type="button" aria-label="Switch to dark theme">${icon('sun', 'theme-sun')}${icon('moon', 'theme-moon')}</button><button class="icon-button mobile-search" data-search-open aria-label="Search documentation" type="button">${icon('search')}</button><button class="icon-button menu-toggle" data-menu-toggle type="button" aria-controls="sidebar" aria-expanded="false" aria-label="Open navigation">${icon('sidepanel')}</button></div></header>
<div class="mobile-bar"><span>${e(page.title)}</span><a href="#on-this-page">On this page ${icon('chevron')}</a></div>
<div class="layout"><aside class="sidebar" id="sidebar">${sidebar(page, ctx)}</aside><button class="nav-scrim" data-menu-close aria-label="Close navigation" tabindex="-1" hidden></button>
<main id="main" class="main ${isOverview ? 'overview' : ''}" tabindex="-1">${isOverview ? '' : `<div class="page-heading"><div class="eyebrow">${e(group)}</div><h1>${e(page.title)}</h1><p class="lead">${description}</p></div>`}${content}
${next ? `<a class="next-page" href="${ctx.url(next.id)}"><span><small>Up next</small><strong>${e(next.title)}</strong></span>${icon('chevron')}</a>` : ''}
<footer class="page-footer"><span>Rosetta <span class="separator">/</span> Vibefield</span><a href="${sourceUrl(page.source ?? 'crates/server/src/http.rs')}">View source ${icon('outward')}</a></footer></main>
<aside class="page-rail" id="on-this-page"><div class="rail-sticky"><h2>On this page</h2><nav aria-label="On this page">${headings.map(({ id, text }) => `<a href="#${id}">${text}</a>`).join('')}</nav><div class="rail-contract"><span class="eyebrow">THE CONTRACT</span><p>Protocol <strong>${ctx.version}</strong></p><p>HTTP + WebSocket</p><a href="${ctx.url('errors')}">Version compatibility ${icon('chevron')}</a></div></div></aside></div>
<dialog class="search-dialog" aria-labelledby="search-title"><div class="search-input-row">${icon('search')}<label class="sr-only" id="search-title" for="docs-search">Search documentation</label><input id="docs-search" type="search" placeholder="Search docs, endpoints, and types…" autocomplete="off" spellcheck="false"><button class="icon-button" data-search-close type="button" aria-label="Close search">${icon('close')}</button></div><div class="search-results" data-search-results></div><div class="search-dialog-foot"><span data-search-status role="status">Start typing to search</span><span><kbd>↑</kbd><kbd>↓</kbd> to move <kbd>↵</kbd> to open <kbd>esc</kbd> to close</span></div></dialog>
<div class="toast" role="status" aria-live="polite" hidden></div>
</body></html>`;
}
