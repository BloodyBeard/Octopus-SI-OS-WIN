'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { DatabaseSync } = require('node:sqlite');

function check(value, message) {
  if (!value) throw Object.assign(new Error(message), { code: 'standard_history_storage_failed' });
}
function canonical(value) {
  return Array.isArray(value) ? `[${value.map(canonical).join(',')}]` : value && typeof value === 'object'
    ? `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${canonical(value[key])}`).join(',')}}` : JSON.stringify(value);
}
function privateDirectory(directory, create = false) {
  check(path.isAbsolute(directory) && path.normalize(directory) === directory, 'History directory must be canonical');
  if (create) try { fs.mkdirSync(directory, { mode: 0o700 }); } catch (error) { if (error.code !== 'EEXIST') throw error; }
  const stat = fs.lstatSync(directory);
  check(stat.isDirectory() && !stat.isSymbolicLink() && stat.uid === process.getuid()
    && (stat.mode & 0o777) === 0o700 && fs.realpathSync(directory) === directory, 'History directory must be private and owned');
  return directory;
}
function childDirectory(parent, name, create) {
  privateDirectory(parent);
  check(typeof name === 'string' && /^[a-zA-Z0-9-]+$/.test(name), 'Invalid history directory name');
  return privateDirectory(path.join(parent, name), create);
}
function openStandardHistoryStorage({ directory, identity, create, maxEntries, maxBytes }) {
  privateDirectory(directory);
  check(typeof create === 'boolean' && identity && typeof identity.api === 'string', 'Explicit history storage identity and purpose required');
  check(Number.isSafeInteger(maxEntries) && maxEntries > 0 && maxEntries <= 100000
    && Number.isSafeInteger(maxBytes) && maxBytes >= 1024 && maxBytes <= 96 * 1024 * 1024, 'Invalid history retention bounds');
  const filename = path.join(directory, 'standard-history.sqlite'), expected = canonical({ identity, maxEntries, maxBytes });
  let created = false;
  if (create) try {
    const fd = fs.openSync(filename, fs.constants.O_CREAT | fs.constants.O_EXCL | fs.constants.O_WRONLY | fs.constants.O_NOFOLLOW, 0o600);
    fs.closeSync(fd); created = true;
  } catch (error) { if (error.code !== 'EEXIST') throw error; }
  const stat = fs.lstatSync(filename);
  check(stat.isFile() && !stat.isSymbolicLink() && stat.nlink === 1 && stat.uid === process.getuid()
    && (stat.mode & 0o777) === 0o600, 'History database must be a private owned regular file');
  const db = new DatabaseSync(filename, { readOnly: !create });
  try {
    db.exec('PRAGMA busy_timeout=2000; PRAGMA trusted_schema=OFF;');
    if (create) db.exec('PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;');
    if (created) {
      db.exec('BEGIN IMMEDIATE; CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL); CREATE TABLE records (id TEXT PRIMARY KEY, occurred_at REAL NOT NULL, body TEXT NOT NULL); CREATE INDEX record_time ON records(occurred_at DESC, id);');
      db.prepare('INSERT INTO metadata VALUES (?, ?)').run('identity', expected);
      db.exec('PRAGMA user_version=1; COMMIT;');
    }
    check(db.prepare('PRAGMA user_version').get().user_version === 1
      && db.prepare('SELECT value FROM metadata WHERE key=?').get('identity')?.value === expected, 'History database belongs to another backend, source, scope or retention contract');
    const size = db.prepare('SELECT COUNT(*) AS count, COALESCE(SUM(length(CAST(body AS BLOB))), 0) AS bytes FROM records').get();
    check(size.count <= maxEntries && size.bytes <= maxBytes, 'History database exceeds its retention contract');
  } catch (error) { db.close(); throw error; }
  let closed = false;
  function current() { check(!closed, 'History database is closed'); }
  function rows() {
    current();
    return db.prepare('SELECT id, occurred_at, body FROM records ORDER BY occurred_at DESC, rowid DESC').all()
      .map(row => ({ id: row.id, occurredAt: row.occurred_at, value: JSON.parse(row.body) }));
  }
  function transaction(operation) {
    current(); check(create, 'History reader cannot mutate storage');
    db.exec('BEGIN IMMEDIATE');
    try { const result = operation(); db.exec('COMMIT'); return result; }
    catch (error) { db.exec('ROLLBACK'); throw error; }
  }
  function put(id, occurredAt, value, protectedIds = new Set()) {
    const body = JSON.stringify(value);
    check(typeof id === 'string' && id.length > 0 && id.length <= 256 && Number.isFinite(occurredAt)
      && typeof body === 'string' && Buffer.byteLength(body) <= maxBytes, 'History record exceeds its contract');
    return transaction(() => {
      db.prepare('INSERT INTO records VALUES (?, ?, ?) ON CONFLICT(id) DO UPDATE SET occurred_at=excluded.occurred_at, body=excluded.body').run(id, occurredAt, body);
      const entries = db.prepare('SELECT id, length(CAST(body AS BLOB)) AS bytes FROM records ORDER BY occurred_at ASC, rowid ASC').all();
      let count = entries.length, bytes = entries.reduce((sum, row) => sum + row.bytes, 0);
      const evicted = [];
      for (const row of entries) {
        if (count <= maxEntries && bytes <= maxBytes) break;
        if (row.id === id || protectedIds.has(row.id)) continue;
        db.prepare('DELETE FROM records WHERE id=?').run(row.id); evicted.push(row.id); count--; bytes -= row.bytes;
      }
      check(count <= maxEntries && bytes <= maxBytes, 'Active history records exhaust the configured retention bound');
      return evicted;
    });
  }
  function remove(ids) {
    check(Array.isArray(ids) && ids.every(id => typeof id === 'string'), 'History deletion requires exact record ids');
    return transaction(() => {
      let removed = 0;
      const statement = db.prepare('DELETE FROM records WHERE id=?');
      for (const id of ids) removed += statement.run(id).changes;
      return removed;
    });
  }
  return Object.freeze({ filename, rows, put, remove, flush: current, close() { if (!closed) { closed = true; db.close(); } } });
}

module.exports = { openStandardHistoryStorage, childDirectory, canonical };
