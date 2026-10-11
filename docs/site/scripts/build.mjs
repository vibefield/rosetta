import { readFile, readdir, mkdir, writeFile, cp, rm } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { execFileSync } from 'node:child_process';
import { endpoints } from '../src/content/endpoints.mjs';
import { guides } from '../src/content/guides.mjs';
import { renderEndpoint, renderPage, renderReference, renderType, renderTypes } from '../src/render.mjs';

export const siteRoot = fileURLToPath(new URL('../', import.meta.url));
export const repoRoot = resolve(siteRoot, '../..');
export const outputRoot = join(siteRoot, 'dist');
const readRepo = path => readFile(join(repoRoot, path), 'utf8');
const assert = (condition, message) => { if (!condition) throw new Error(message); };

// A whitespace-only formatter for generated declarations. String literals and
// comments stay intact; this never reinterprets or edits the wire contract.
export function formatType(source) {
  const declaration = source.slice(source.indexOf('export type '));
  const tokens = declaration.match(/\/\*[\s\S]*?\*\/|"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|[{};,]|[^{};,/"']+|\//g) ?? [];
  let indent = 0, text = '', lineStart = true;
  const append = word => { if (lineStart) text += '  '.repeat(indent); text += word; lineStart = false; };
  const newline = () => { text = text.trimEnd() + '\n'; lineStart = true; };
  for (const token of tokens) {
    if (token === '{') { append('{'); indent++; newline(); }
    else if (token === '}') { indent = Math.max(0, indent - 1); if (!lineStart) newline(); append('}'); }
    else if (token === ',' || token === ';') { append(token); newline(); }
    else if (token.startsWith('/*')) {
      if (!lineStart) newline();
      for (const line of token.split('\n')) { append(line.trim()); newline(); }
    } else {
      const clean = token.replace(/\s+/g, ' ');
      if (clean.trim()) append(lineStart ? clean.trimStart() : clean);
    }
  }
  return text.trim();
}

async function loadTypes() {
  const root = join(repoRoot, 'clients/ts/generated');
  const files = (await readdir(root, { recursive: true })).filter(file => file.endsWith('.ts')).sort();
  const types = new Map();
  for (const file of files) {
    const source = await readFile(join(root, file), 'utf8');
    const name = source.match(/export type (\w+)/)?.[1];
    assert(name, `Missing generated declaration in ${file}`);
    const dependencies = [...source.matchAll(/import type \{ (\w+) \}/g)].map(match => match[1]);
    const before = source.slice(0, source.indexOf('export type '));
    const summary = [...before.matchAll(/\/\*\*([\s\S]*?)\*\//g)].at(-1)?.[1].replace(/^\s*\* ?/gm, '').trim().split('\n\n')[0].replace(/\s+/g, ' ') ?? '';
    types.set(name, { source, dependencies, summary, file: `clients/ts/generated/${file.replaceAll('\\', '/')}` });
  }
  return new Map([...types].sort(([a], [b]) => a.localeCompare(b)));
}

export async function build({ check = false } = {}) {
  for (const file of ['app.js', 'theme.js']) execFileSync(process.execPath, ['--check', join(siteRoot, 'src', file)]);
  const [router, protocol, clientVersion, client, types, cargo, toolchain, publicIndex] = await Promise.all([
    readRepo('crates/server/src/http.rs'), readRepo('crates/protocol/src/lib.rs'), readRepo('clients/ts/protocol-version.mjs'), readRepo('clients/ts/client.ts'), loadTypes(), readRepo('Cargo.toml'), readRepo('rust-toolchain.toml'), readRepo('clients/ts/index.ts'),
  ]);
  const version = Number(protocol.match(/pub const PROTOCOL_VERSION: u32 = (\d+)/)?.[1]);
  assert(version === Number(clientVersion.match(/PROTOCOL_VERSION = (\d+)/)?.[1]), 'Rust and TypeScript protocol versions differ.');
  const routes = [...router.matchAll(/\.route\(\s*"([^"]+)"\s*,\s*(get|axum::routing::post)\s*\(/g)].map(([, path, method]) => `${path.startsWith('/ws/') ? 'WS' : method === 'get' ? 'GET' : 'POST'} ${path}`);
  const documentedRoutes = endpoints.map(ep => `${ep.method} ${ep.path}`);
  assert(new Set(documentedRoutes).size === endpoints.length, 'Duplicate documented route.');
  assert(routes.length === (router.match(/\.route\(/g) ?? []).length, 'Router parser did not recognize every route. Update the docs source check.');
  for (const route of routes) assert(documentedRoutes.includes(route), `Undocumented route: ${route}`);
  for (const endpoint of endpoints) {
    assert(routes.includes(`${endpoint.method} ${endpoint.path}`), `Documented route no longer exists: ${endpoint.path}`);
    for (const name of [endpoint.request, endpoint.response].filter(Boolean)) assert(['binary', 'document'].includes(name) || types.has(name), `Unknown response/request type: ${name}`);
    const method = endpoint.sdk.match(/client\.(\w+)/)?.[1];
    assert(method && new RegExp(`\\b${method}\\(`).test(client), `SDK method no longer exists: ${method}`);
  }
  for (const [name, info] of types) for (const dep of info.dependencies) assert(types.has(dep), `${name} references missing type ${dep}`);
  const publicTypes = new Set([...publicIndex.matchAll(/export (?:type )?\* from "\.\/generated\/(\w+)"/g)].map(match => match[1]));
  assert(publicTypes.size > 0, 'No public generated types found in the package exports.');
  const pages = [
    ...guides,
    { id: 'reference', title: 'API reference', group: 'Reference', description: `${endpoints.length} endpoints for local agent history. Read the contract, find a route, and make a request.`, render: renderReference },
    ...endpoints.map(ep => ({ id: `reference/${ep.id}`, title: ep.title, group: ep.group, description: ep.description, source: ep.method === 'WS' ? 'crates/server/src/ws.rs' : 'crates/server/src/http.rs', render: ctx => renderEndpoint(ep, ctx) })),
    { id: 'types', title: 'Type reference', group: 'Contract', description: `${types.size} generated definitions. The same types your application imports, with the relationships you need to navigate them.`, source: 'clients/ts/index.ts', render: renderTypes },
    ...[...types].map(([name, info]) => ({ id: `types/${name}`, title: name, group: 'Type reference', description: info.summary || `The generated ${name} wire definition.`, source: info.file, render: ctx => renderType(name, ctx) })),
  ];
  assert(new Set(pages.map(page => page.id)).size === pages.length, 'Duplicate page id.');
  const index = [], rendered = new Map();
  const shared = { endpoints, guides, types, publicTypes, version, count: endpoints.length, typeCount: types.size, rustVersion: toolchain.match(/channel = "([^"]+)"/)[1], release: cargo.match(/\[workspace.package\]\nversion = "([^"]+)"/)[1], formattedType: name => formatType(types.get(name).source), pageTitle: id => pages.find(page => page.id === id)?.title ?? id };
  for (const page of pages) {
    const depth = page.id ? page.id.split('/').length : 0;
    const root = '../'.repeat(depth) || './';
    const ctx = { ...shared, url: id => `${root}${id ? `${id}/` : ''}`, asset: name => `${root}assets/${name}` };
    const content = page.render(ctx);
    const html = renderPage(page, content, ctx);
    rendered.set(page.id, html);
    const text = content.replace(/<[^>]+>/g, ' ').replace(/\s+/g, ' ');
    index.push({ id: page.id, title: page.title, group: page.group, description: page.description, text });
  }
  // Check every local destination and fragment, including generated type links.
  for (const [id, html] of rendered) {
    const url = new URL(`https://docs.invalid/${id ? `${id}/` : ''}`);
    const seen = [...html.matchAll(/\bid="([^"]+)"/g)].map(match => match[1]);
    assert(seen.length === new Set(seen).size, `Duplicate HTML id in ${id || 'overview'}`);
    for (const [, href] of html.matchAll(/href="([^"]+)"/g)) {
      const target = new URL(href, url);
      if (target.origin !== url.origin || target.pathname.startsWith('/assets/')) continue;
      const page = target.pathname.replace(/^\/|\/$/g, '');
      assert(rendered.has(page), `Broken link in ${id}: ${href}`);
      if (target.hash) assert(rendered.get(page).includes(`id="${target.hash.slice(1)}"`), `Broken fragment in ${id}: ${href}`);
    }
  }
  if (!check) {
    // Only generated output lives here. Removed pages must not survive a build.
    await rm(outputRoot, { recursive: true, force: true });
    await mkdir(join(outputRoot, 'assets'), { recursive: true });
    for (const [id, html] of rendered) {
      const directory = join(outputRoot, id);
      await mkdir(directory, { recursive: true });
      await writeFile(join(directory, 'index.html'), html);
    }
    await cp(join(siteRoot, 'src/style.css'), join(outputRoot, 'assets/style.css'));
    await cp(join(siteRoot, 'src/app.js'), join(outputRoot, 'assets/app.js'));
    await cp(join(siteRoot, 'src/theme.js'), join(outputRoot, 'assets/theme.js'));
    await cp(join(siteRoot, 'src/favicon.svg'), join(outputRoot, 'assets/favicon.svg'));
    // Rosetta's existing transcription of the Vibefield design-kit is shared,
    // not forked into a second theme. A clean checkout needs no sibling repos.
    await cp(join(repoRoot, 'clients/app/renderer/src/styles/tokens.css'), join(outputRoot, 'assets/tokens.css'));
    await writeFile(join(outputRoot, 'assets/search.json'), JSON.stringify(index));
    await writeFile(join(outputRoot, '.nojekyll'), '');
  }
  const result = { pages: rendered.size, endpoints: endpoints.length, types: types.size, version };
  console.log(`${check ? 'Checked' : 'Built'} ${result.pages} pages · ${result.endpoints} routes · ${result.types} types · protocol ${version}`);
  return result;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  await build({ check: process.argv.includes('--check') });
}
