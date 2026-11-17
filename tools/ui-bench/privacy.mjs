// The ui-run/1 privacy gate. run_ui.mjs calls it before the JSON is written and refuses
// to write on any error. Two independent checks:
//   1. a whitelist schema (ui-run-1.schema.json): every key must be declared, and a
//      string is allowed only where the schema says enum (one of its values), sha256
//      (64 hex) or a fixed identity form (machine model, OS version, window size,
//      ISO time, a version). Numbers and booleans are free; nothing else is.
//   2. a forbidden-value scan over the strings the schema does not fix (the sha256 and
//      identity positions; an enum value is one of the schema's own strings and can
//      carry nothing else): none may equal or contain a value the harness knows is
//      private (session/project/runtime ids, native ids, titles, cwds, names, paths,
//      query text), so a 64-hex id cannot pass as a digest.
// Schema forms: "num" "int" "bool" "sha256" (a trailing ? admits null);
// {"enum":[...], "null":bool}; {"id": form, "null":bool}; {"obj":{k: schema}, "opt":[k]};
// {"map": schema, "keys":[allowed]}; {"arr": schema, "max": n}; {"tuple":[schema...]};
// {"ref": name} into "defs"; {"anyOf":[schema...]}.
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

export const SCHEMA_PATH = fileURLToPath(new URL('./ui-run-1.schema.json', import.meta.url));
const IDENTITY = {
  'machine-model': /^[A-Za-z]+[0-9]+,[0-9]+$/,
  'os-version': /^[0-9]+(\.[0-9]+){0,3}$/,
  window: /^[0-9]{3,5}x[0-9]{3,5}$/,
  'iso-time': /^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]{1,3})?Z$/,
  version: /^[0-9]+\.[0-9]+\.[0-9]+$/,
};
const SHA256 = /^[0-9a-f]{64}$/;
// An error names a refused key only by its length and a hash prefix: the key itself
// may be the private value the gate caught, and errors are logged.
const keyRef = (k) => `(${String(k).length} chars, sha256 ${createHash('sha256').update(String(k)).digest('hex').slice(0, 8)})`;
const own = (o, k) => Object.prototype.hasOwnProperty.call(o, k);

export function loadSchema(file = SCHEMA_PATH) {
  return JSON.parse(readFileSync(file, 'utf8'));
}

function check(value, schema, at, defs, errors, free) {
  if (errors.length > 50) return;
  const fail = (why) => errors.push(`${at}: ${why}`);
  if (schema === null || schema === undefined || (typeof schema !== 'string' && typeof schema !== 'object')) { fail('schema error: missing node'); return; }
  if (typeof schema === 'string') {
    const nullable = schema.endsWith('?');
    const kind = nullable ? schema.slice(0, -1) : schema;
    if (value === null) { if (!nullable) fail('null not allowed'); return; }
    if (kind === 'num') { if (typeof value !== 'number' || !Number.isFinite(value)) fail('expected a finite number'); return; }
    if (kind === 'int') { if (!Number.isInteger(value)) fail('expected an integer'); return; }
    if (kind === 'bool') { if (typeof value !== 'boolean') fail('expected a boolean'); return; }
    if (kind === 'sha256') { if (typeof value !== 'string' || !SHA256.test(value)) fail('expected a sha256 hex digest'); else free.push(value); return; }
    fail(`schema error: unknown form ${schema}`);
    return;
  }
  if (schema.ref) { check(value, own(defs, schema.ref) ? defs[schema.ref] : null, at, defs, errors, free); return; }
  if (schema.anyOf) {
    for (const s of schema.anyOf) { const e = []; const f = []; check(value, s, at, defs, e, f); if (e.length === 0) { free.push(...f); return; } }
    fail('matches no allowed form');
    return;
  }
  if (value === null) { if (!schema.null) fail('null not allowed'); return; }
  if (schema.enum) { if (!schema.enum.includes(value)) fail(typeof value === 'string' ? 'string is not a whitelisted enum value' : 'not an allowed value'); return; }
  if (schema.id) {
    if (typeof value !== 'string' || !IDENTITY[schema.id] || !IDENTITY[schema.id].test(value)) fail(`not a ${schema.id} identity`);
    else free.push(value);
    return;
  }
  if (schema.obj) {
    if (typeof value !== 'object' || Array.isArray(value)) { fail('expected an object'); return; }
    const opt = new Set(schema.opt || []);
    // Own keys only: `k in` would admit Object.prototype names (constructor,
    // toString, __proto__ …) as declared keys and never look at their values.
    for (const k of Object.keys(value)) if (!own(schema.obj, k)) fail(`key ${keyRef(k)} is not in the schema`);
    for (const [k, s] of Object.entries(schema.obj)) {
      if (!own(value, k)) { if (!opt.has(k)) fail(`missing key ${k}`); continue; }
      check(value[k], s, `${at}.${k}`, defs, errors, free);
    }
    return;
  }
  if (schema.map) {
    if (typeof value !== 'object' || Array.isArray(value)) { fail('expected an object'); return; }
    for (const [k, v] of Object.entries(value)) {
      if (!schema.keys.includes(k)) { fail(`key ${keyRef(k)} is not whitelisted`); continue; }
      check(v, schema.map, `${at}.${k}`, defs, errors, free);
    }
    return;
  }
  if (schema.arr) {
    if (!Array.isArray(value)) { fail('expected an array'); return; }
    if (schema.max !== undefined && value.length > schema.max) fail(`more than ${schema.max} items`);
    value.forEach((v, i) => check(v, schema.arr, `${at}[${i}]`, defs, errors, free));
    return;
  }
  if (schema.tuple) {
    if (!Array.isArray(value) || value.length !== schema.tuple.length) { fail(`expected a ${schema.tuple.length}-tuple`); return; }
    value.forEach((v, i) => check(v, schema.tuple[i], `${at}[${i}]`, defs, errors, free));
    return;
  }
  fail('schema error: unknown node');
}

/** Returns { ok, errors, freeStrings }. `forbidden`: private values no schema-free
 * string may equal or (for values of 8 characters or more) contain or be contained in.
 * Every free string is a 64-hex digest or a short identity form, so a 64-hex private
 * value (a rosetta id: session, project, runtime, timeline) can only match by equality,
 * and only the others need the substring scan; the scan therefore stays cheap with
 * tens of thousands of ids. */
export function validateRun(run, { schema = loadSchema(), forbidden = [] } = {}) {
  const errors = [];
  const free = [];
  check(run, schema.root, '$', schema.defs || {}, errors, free);
  // A ui-run/1 document reads exactly one source: the R1 snapshot or a fixture home.
  const id = run?.format === 'ui-run/1' && run.identity && typeof run.identity === 'object' ? run.identity : null;
  if (id && [id.snapshot, id.fixture].filter((x) => x !== null && x !== undefined).length !== 1) {
    errors.push('$.identity: exactly one of snapshot and fixture must be set');
  }
  const exact = new Set();
  const partial = [];
  for (const f of forbidden) {
    if (typeof f !== 'string' || f.length === 0) continue;
    exact.add(f);
    if (f.length >= 8 && !SHA256.test(f)) partial.push(f);
  }
  for (const s of free) {
    if (exact.has(s) || partial.some((f) => s.includes(f) || (s.length >= 8 && f.includes(s)))) {
      errors.push('a digest or identity field carries a forbidden private value');
      break;
    }
  }
  return { ok: errors.length === 0, errors, freeStrings: free.length };
}
