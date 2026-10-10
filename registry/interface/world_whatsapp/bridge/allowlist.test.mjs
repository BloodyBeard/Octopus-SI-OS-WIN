import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import {
  expandAliases,
  matchesIdentifier,
  normalizeIdentifier,
  normalizeJid,
  parseIdentifierSet,
} from './allowlist.mjs';

test('normalizes inbound identities and outbound JIDs', () => {
  assert.equal(normalizeIdentifier('+15551234567:8@s.whatsapp.net'), '15551234567');
  assert.equal(normalizeJid('+1 (555) 123-4567'), '15551234567@s.whatsapp.net');
  assert.equal(normalizeJid('12345@g.us'), '12345@g.us');
});

test('rejects an empty allowlist and honors an explicit wildcard', () => {
  const session = mkdtempSync(path.join(os.tmpdir(), 'octopus-wa-'));
  assert.equal(matchesIdentifier('15551234567@s.whatsapp.net', parseIdentifierSet(''), session), false);
  assert.equal(matchesIdentifier('15551234567@s.whatsapp.net', parseIdentifierSet('*'), session), true);
});

test('resolves phone and LID aliases transitively', () => {
  const session = mkdtempSync(path.join(os.tmpdir(), 'octopus-wa-'));
  writeFileSync(path.join(session, 'lid-mapping-15551234567.json'), JSON.stringify('998877665544'));
  writeFileSync(path.join(session, 'lid-mapping-998877665544_reverse.json'), JSON.stringify('15551234567'));
  assert.deepEqual([...expandAliases('998877665544@lid', session)].sort(), ['15551234567', '998877665544']);
  assert.equal(matchesIdentifier('998877665544@lid', parseIdentifierSet('15551234567'), session), true);
});
