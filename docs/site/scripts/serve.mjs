import { createServer } from 'node:http';
import { readFile, stat } from 'node:fs/promises';
import { watch } from 'node:fs';
import { spawn } from 'node:child_process';
import { extname, join, resolve, sep } from 'node:path';
import { build, outputRoot, repoRoot, siteRoot } from './build.mjs';

const watching = process.argv.includes('--watch');
const port = Number(process.env.PORT ?? 5190);
if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error('PORT must be an integer from 1 to 65535.');
await build();
const streams = new Set();
const mime = { '.html': 'text/html; charset=utf-8', '.css': 'text/css; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.json': 'application/json; charset=utf-8', '.svg': 'image/svg+xml' };
const server = createServer(async (request, response) => {
  if (!['GET', 'HEAD'].includes(request.method)) { response.writeHead(405, { Allow: 'GET, HEAD' }); response.end(); return; }
  try {
    const url = new URL(request.url, 'http://localhost');
    if (watching && url.pathname === '/__docs/events') {
      response.writeHead(200, { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-cache', Connection: 'keep-alive' });
      response.write(': connected\n\n');
      streams.add(response);
      request.on('close', () => streams.delete(response));
      return;
    }
    if (watching && url.pathname === '/__docs/reload.js') {
      response.writeHead(200, { 'Content-Type': mime['.js'] });
      response.end('new EventSource("/__docs/events").onmessage = () => location.reload();');
      return;
    }
    let path;
    try { path = resolve(outputRoot, `.${decodeURIComponent(url.pathname)}`); }
    catch { response.writeHead(400); response.end('Invalid path'); return; }
    if (path !== outputRoot && !path.startsWith(outputRoot + sep)) { response.writeHead(403); response.end('Forbidden'); return; }
    const info = await stat(path);
    if (info.isDirectory()) {
      if (!url.pathname.endsWith('/')) { response.writeHead(308, { Location: url.pathname + '/' + url.search }); response.end(); return; }
      path = join(path, 'index.html');
    }
    let body = await readFile(path);
    if (watching && extname(path) === '.html') body = Buffer.from(body.toString().replace('</body>', '<script src="/__docs/reload.js"></script></body>'));
    response.writeHead(200, { 'Content-Type': mime[extname(path)] ?? 'application/octet-stream', 'Content-Length': body.length, 'Cache-Control': 'no-store', 'X-Content-Type-Options': 'nosniff' });
    response.end(request.method === 'HEAD' ? undefined : body);
  } catch (error) {
    response.writeHead(error.code === 'ENOENT' ? 404 : 500, { 'Content-Type': 'text/html; charset=utf-8' });
    response.end('<!doctype html><html lang="en"><title>Page not found — Rosetta</title><h1>Page not found</h1><p><a href="/">Return to the documentation</a></p></html>');
  }
});
server.listen(port, '127.0.0.1', () => console.log(`Rosetta docs: http://127.0.0.1:${port}${watching ? ' (watching source)' : ''}`));
server.on('error', error => { console.error(error.message); process.exitCode = 1; for (const watcher of watchers) watcher.close(); });

const watchers = [];
let timer, building = false, dirty = false;
function rebuild() {
  if (building) { dirty = true; return; }
  building = true;
  const child = spawn(process.execPath, [join(siteRoot, 'scripts/build.mjs')], { stdio: 'inherit' });
  child.on('exit', code => {
    building = false;
    if (code === 0) for (const stream of streams) stream.write('data: reload\n\n');
    if (dirty) { dirty = false; rebuild(); }
  });
}
if (watching) {
  for (const directory of ['docs/site/src', 'docs/site/scripts', 'clients/ts', 'crates/protocol/src', 'crates/server/src', 'clients/app/renderer/src/styles']) {
    watchers.push(watch(join(repoRoot, directory), { recursive: true }, () => { clearTimeout(timer); timer = setTimeout(rebuild, 180); }));
  }
}
function shutdown() {
  clearTimeout(timer);
  for (const watcher of watchers) watcher.close();
  for (const stream of streams) stream.end();
  server.close();
}
process.on('SIGINT', shutdown);
process.on('SIGTERM', shutdown);
