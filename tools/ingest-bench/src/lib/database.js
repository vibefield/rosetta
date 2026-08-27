const fs = require('node:fs');
const path = require('node:path');
const { DatabaseSync } = require('node:sqlite');

const MAX_QUERY_LENGTH = 20_000;
const MAX_ROWS = 1_000;
const MAX_CELL_CHARS = 100_000;

function quoteIdentifier(name) {
  if (typeof name !== 'string' || !name) throw new Error('object name is required');
  return `"${name.replaceAll('"', '""')}"`;
}

function safeValue(value) {
  if (typeof value === 'bigint') {
    return value <= BigInt(Number.MAX_SAFE_INTEGER) && value >= BigInt(Number.MIN_SAFE_INTEGER)
      ? Number(value)
      : value.toString();
  }
  if (Buffer.isBuffer(value) || value instanceof Uint8Array) {
    const bytes = Buffer.from(value);
    return {
      $type: 'blob',
      bytes: bytes.length,
      hex: bytes.subarray(0, 256).toString('hex'),
      truncated: bytes.length > 256,
    };
  }
  if (typeof value === 'string' && value.length > MAX_CELL_CHARS) {
    return {
      $type: 'text',
      chars: value.length,
      text: value.slice(0, MAX_CELL_CHARS),
      truncated: true,
    };
  }
  return value;
}

function safeRows(rows) {
  return rows.map((row) => Object.fromEntries(Object.entries(row).map(([key, value]) => [key, safeValue(value)])));
}

function resultFor(statement, rows, extra = {}) {
  return {
    columns: statement.columns().map((column) => column.name),
    rows: safeRows(rows),
    ...extra,
  };
}

class ProjectionDatabase {
  constructor() {
    this.db = null;
    this.file = null;
  }

  close() {
    this.db?.close();
    this.db = null;
    this.file = null;
  }

  open(file) {
    this.close();
    const resolved = path.resolve(file);
    const stat = fs.statSync(resolved);
    if (!stat.isFile()) throw new Error(`not a SQLite file: ${resolved}`);
    this.db = new DatabaseSync(resolved, { readOnly: true });
    this.db.enableDefensive(true);
    this.db.exec('PRAGMA query_only = ON; PRAGMA busy_timeout = 5000;');
    const header = this.db.prepare('PRAGMA user_version').get();
    this.file = resolved;
    return { file: resolved, bytes: stat.size, schemaVersion: safeValue(header.user_version) };
  }

  requireOpen() {
    if (!this.db || !this.file) throw new Error('open a projection first');
  }

  overview() {
    this.requireOpen();
    const pragma = (name) => Object.values(this.db.prepare(`PRAGMA ${name}`).get() || {})[0] ?? null;
    const counts = {};
    for (const table of [
      'agent_runtime',
      'project',
      'session',
      'message',
      'file_edit',
      'git_commit',
      'drift_agg',
      'pending_merge',
      'source_op',
    ]) {
      try {
        counts[table] = safeValue(this.db.prepare(`SELECT count(*) AS count FROM ${quoteIdentifier(table)}`).get().count);
      } catch {
        counts[table] = null;
      }
    }
    const objects = safeRows(
      this.db.prepare(`
        SELECT type, name, tbl_name AS tableName, sql
        FROM sqlite_schema
        WHERE name NOT LIKE 'sqlite_%'
        ORDER BY CASE type WHEN 'table' THEN 0 WHEN 'view' THEN 1 WHEN 'index' THEN 2 ELSE 3 END, name
      `).all(),
    );
    const stat = fs.statSync(this.file);
    return {
      file: this.file,
      bytes: stat.size,
      schemaVersion: safeValue(pragma('user_version')),
      journalMode: pragma('journal_mode'),
      pageSize: safeValue(pragma('page_size')),
      pageCount: safeValue(pragma('page_count')),
      freelistCount: safeValue(pragma('freelist_count')),
      counts,
      objects,
    };
  }

  table(name, { limit = 100, offset = 0 } = {}) {
    this.requireOpen();
    const object = this.db
      .prepare("SELECT type, name, sql FROM sqlite_schema WHERE name = ? AND type IN ('table', 'view')")
      .get(name);
    if (!object) throw new Error(`no table or view named ${name}`);
    const boundedLimit = Math.max(1, Math.min(MAX_ROWS, Number(limit) || 100));
    const boundedOffset = Math.max(0, Number(offset) || 0);
    const identifier = quoteIdentifier(name);
    const total = this.db.prepare(`SELECT count(*) AS count FROM ${identifier}`).get().count;
    const statement = this.db.prepare(`SELECT * FROM ${identifier} LIMIT ? OFFSET ?`);
    return resultFor(statement, statement.all(boundedLimit, boundedOffset), {
      name,
      type: object.type,
      sql: object.sql,
      total: safeValue(total),
      limit: boundedLimit,
      offset: boundedOffset,
    });
  }

  query(sql) {
    this.requireOpen();
    if (typeof sql !== 'string' || !sql.trim()) throw new Error('enter a read-only SQL statement');
    if (sql.length > MAX_QUERY_LENGTH) throw new Error(`SQL is limited to ${MAX_QUERY_LENGTH} characters`);
    const trimmed = sql.trim().replace(/;+\s*$/, '');
    if (!/^(select|with|explain|pragma)\b/i.test(trimmed)) {
      throw new Error('only SELECT, WITH, EXPLAIN, and PRAGMA statements are allowed');
    }
    const isRowQuery = /^(select|with)\b/i.test(trimmed);
    const preparedSql = isRowQuery ? `SELECT * FROM (${trimmed}) LIMIT ${MAX_ROWS}` : trimmed;
    const statement = this.db.prepare(preparedSql);
    const rows = statement.all();
    return resultFor(statement, rows, { limitedTo: isRowQuery ? MAX_ROWS : null });
  }
}

module.exports = {
  MAX_ROWS,
  ProjectionDatabase,
  quoteIdentifier,
  safeValue,
};
