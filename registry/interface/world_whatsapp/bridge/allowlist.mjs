import { existsSync, readFileSync } from 'node:fs';
import path from 'node:path';

const SAFE_ID = /^[A-Za-z0-9@.+-]+$/;

export function normalizeIdentifier(value) {
  return String(value || '')
    .trim()
    .replace(/^\+/, '')
    .replace(/:.*@/, '@')
    .replace(/@.*/, '');
}

export function normalizeJid(value) {
  const raw = String(value || '').trim();
  if (!raw) return '';
  if (raw.includes('@')) {
    const [prefix, domain] = raw.split('@', 2);
    return `${prefix.split(':', 1)[0]}@${domain}`;
  }
  const digits = raw.replace(/[^0-9]/g, '');
  return digits ? `${digits}@s.whatsapp.net` : raw;
}

export function parseIdentifierSet(raw) {
  if (Array.isArray(raw)) {
    return new Set(raw.map(normalizeIdentifier).filter(Boolean));
  }
  return new Set(String(raw || '').split(',').map(normalizeIdentifier).filter(Boolean));
}

function readAlias(sessionDir, identifier, suffix) {
  if (!SAFE_ID.test(identifier)) return null;
  const target = path.join(sessionDir, `lid-mapping-${identifier}${suffix}.json`);
  if (!existsSync(target)) return null;
  try {
    return normalizeIdentifier(JSON.parse(readFileSync(target, 'utf8')));
  } catch {
    return null;
  }
}

export function expandAliases(value, sessionDir) {
  const first = normalizeIdentifier(value);
  if (!first) return new Set();
  const queue = [first];
  const resolved = new Set();
  while (queue.length) {
    const current = queue.shift();
    if (!current || resolved.has(current) || !SAFE_ID.test(current)) continue;
    resolved.add(current);
    for (const suffix of ['', '_reverse']) {
      const mapped = readAlias(sessionDir, current, suffix);
      if (mapped && !resolved.has(mapped)) queue.push(mapped);
    }
  }
  return resolved;
}

export function matchesIdentifier(value, allowed, sessionDir) {
  if (!(allowed instanceof Set) || allowed.size === 0) return false;
  if (allowed.has('*')) return true;
  for (const alias of expandAliases(value, sessionDir)) {
    if (allowed.has(alias)) return true;
  }
  return false;
}
