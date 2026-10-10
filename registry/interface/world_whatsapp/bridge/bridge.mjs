#!/usr/bin/env node
import {
  DisconnectReason,
  downloadMediaMessage,
  fetchLatestBaileysVersion,
  generateWAMessageFromContent,
  getAggregateVotesInPollMessage,
  makeWASocket,
  proto,
  useMultiFileAuthState,
} from '@whiskeysockets/baileys';
import { Boom } from '@hapi/boom';
import { HttpsProxyAgent } from 'https-proxy-agent';
import {
  closeSync,
  constants,
  existsSync,
  fstatSync,
  fsyncSync,
  lstatSync,
  linkSync,
  mkdirSync,
  openSync,
  readSync,
  readFileSync,
  readdirSync,
  renameSync,
  rmSync,
  unlinkSync,
  writeFileSync,
} from 'node:fs';
import { createHash, randomBytes } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { isIP } from 'node:net';
import path from 'node:path';
import readline from 'node:readline';
import pino from 'pino';
import {
  matchesIdentifier,
  normalizeIdentifier,
  normalizeJid,
  parseIdentifierSet,
} from './allowlist.mjs';
import { sanitizeExternalError } from './redaction.mjs';

process.umask(0o077);

function fail(message, code = 'bridge_error', details = {}) {
  const error = new Error(message);
  error.code = code;
  error.details = details;
  return error;
}

function requiredString(value, name, allowEmpty = false) {
  if (typeof value !== 'string' || (!allowEmpty && !value.trim())) throw fail(`${name} must be a non-empty string`, 'boundary_rejected');
  return value;
}

function requiredObject(value, name) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw fail(`${name} must be an object`, 'boundary_rejected');
  return value;
}

function requiredInteger(value, name, minimum = 0, maximum = Number.MAX_SAFE_INTEGER) {
  if (!Number.isSafeInteger(value) || value < minimum || value > maximum) throw fail(`${name} must be an integer in range`, 'boundary_rejected');
  return value;
}

function envInteger(name, fallback, minimum, maximum) {
  const raw = process.env[name];
  if (raw === undefined || raw === '') return fallback;
  const parsed = Number(raw);
  return requiredInteger(parsed, name, minimum, maximum);
}

function requirePrivateMode(info, expected, name) {
  if (process.platform !== 'win32' && (info.mode & 0o777) !== expected) throw fail(`${name} must have mode ${expected.toString(8).padStart(4, '0')}`, 'private_state_permissions');
}

function ensurePrivateDirectory(target) {
  mkdirSync(target, { recursive: true, mode: 0o700 });
  const info = lstatSync(target);
  if (info.isSymbolicLink() || !info.isDirectory()) throw fail(`${target} is not a private directory`, 'private_state_permissions');
  requirePrivateMode(info, 0o700, target);
  return target;
}

function openRegularNoFollow(target, requirePrivate) {
  const before = lstatSync(target);
  if (before.isSymbolicLink() || !before.isFile()) throw fail(`${target} is not a regular file`, 'private_state_permissions');
  if (requirePrivate) requirePrivateMode(before, 0o600, target);
  const fd = openSync(target, constants.O_RDONLY | (constants.O_NOFOLLOW || 0));
  try {
    const current = fstatSync(fd);
    if (!current.isFile() || before.dev !== current.dev || before.ino !== current.ino) throw fail(`${target} changed while opening`, 'private_state_permissions');
    if (requirePrivate) requirePrivateMode(current, 0o600, target);
    return fd;
  } catch (error) {
    closeSync(fd);
    throw error;
  }
}

function readBoundedFd(fd, maximumBytes, name) {
  const chunks = [];
  const buffer = Buffer.allocUnsafe(Math.min(65536, maximumBytes + 1));
  let observed = 0;
  while (true) {
    const read = readSync(fd, buffer, 0, Math.min(buffer.length, maximumBytes + 1 - observed), null);
    if (read === 0) break;
    observed += read;
    if (observed > maximumBytes) throw fail(`${name} exceeds its byte ceiling`, 'media_too_large', { observed });
    chunks.push(Buffer.from(buffer.subarray(0, read)));
  }
  return Buffer.concat(chunks, observed);
}

function readRegularNoFollow(target, requirePrivate = false, maximumBytes = null) {
  const fd = openRegularNoFollow(target, requirePrivate);
  try {
    return maximumBytes === null ? readFileSync(fd) : readBoundedFd(fd, maximumBytes, target);
  } finally {
    closeSync(fd);
  }
}

function digestRegularNoFollow(target, maximumBytes, requirePrivate = false) {
  const fd = openRegularNoFollow(target, requirePrivate);
  try {
    const size = fstatSync(fd).size;
    if (!Number.isSafeInteger(size) || size < 0 || size > maximumBytes) throw fail(`${target} exceeds its byte ceiling`, 'media_too_large', { observed: size });
    const digest = createHash('sha256');
    const buffer = Buffer.allocUnsafe(65536);
    let observed = 0;
    while (observed < size) {
      const read = readSync(fd, buffer, 0, Math.min(buffer.length, size - observed), null);
      if (read === 0) throw fail(`${target} ended before its declared size`, 'private_state_corrupt');
      observed += read;
      digest.update(buffer.subarray(0, read));
    }
    if (readSync(fd, buffer, 0, 1, null) !== 0) throw fail(`${target} changed while hashing`, 'private_state_corrupt');
    return { digest: digest.digest('hex'), size: observed };
  } finally {
    closeSync(fd);
  }
}

function privateFileMtime(target) {
  const fd = openRegularNoFollow(target, true);
  try {
    return fstatSync(fd).mtimeMs;
  } finally {
    closeSync(fd);
  }
}

function validatePrivateTree(root) {
  ensurePrivateDirectory(root);
  for (const entry of readdirSync(root, { withFileTypes: true })) {
    const target = path.join(root, entry.name);
    if (entry.isSymbolicLink()) throw fail(`${target} is a symlink inside private state`, 'private_state_permissions');
    if (entry.isDirectory()) validatePrivateTree(target);
    else if (entry.isFile()) readRegularNoFollow(target, true);
    else throw fail(`${target} is unsupported private state`, 'private_state_permissions');
  }
  return root;
}

function validateProxyUrl(value) {
  const raw = String(value || '');
  if (!raw) return '';
  if (raw.trim() !== raw || /[\x00-\x20\x7f\\]/u.test(raw) || raw.includes('?') || raw.includes('#')) {
    throw fail('WhatsApp proxy URL contains invalid transport data', 'boundary_rejected');
  }
  let parsed;
  try {
    parsed = new URL(raw);
  } catch {
    throw fail('WhatsApp proxy URL is malformed', 'boundary_rejected');
  }
  if (!['http:', 'https:'].includes(parsed.protocol) || !parsed.hostname || !['', '/'].includes(parsed.pathname)) {
    throw fail('WhatsApp proxy URL must be an HTTP or HTTPS origin', 'boundary_rejected');
  }
  let username;
  let password;
  try {
    username = parsed.username ? decodeURIComponent(parsed.username) : '';
    password = parsed.password ? decodeURIComponent(parsed.password) : '';
  } catch {
    throw fail('WhatsApp proxy credentials are malformed', 'boundary_rejected');
  }
  if ((!username && parsed.password) || /[\x00-\x1f\x7f]/u.test(username) || /[\x00-\x1f\x7f]/u.test(password)) {
    throw fail('WhatsApp proxy credentials are invalid', 'boundary_rejected');
  }
  const host = parsed.hostname.replace(/^\[|\]$/g, '').replace(/\.$/u, '').toLowerCase();
  const ipType = isIP(host);
  const loopback = host === 'localhost' || (ipType === 4 && host.split('.')[0] === '127') || (ipType === 6 && host === '::1');
  if (parsed.protocol === 'http:' && !loopback) {
    throw fail('remote WhatsApp proxies must use HTTPS', 'boundary_rejected');
  }
  return raw;
}

function atomicJson(target, value) {
  const dir = path.dirname(target);
  ensurePrivateDirectory(dir);
  const temporary = path.join(dir, `.${path.basename(target)}.${process.pid}.${Date.now()}.${randomBytes(4).toString('hex')}.tmp`);
  const body = `${JSON.stringify(value, null, 2)}\n`;
  const fd = openSync(temporary, 'wx', 0o600);
  try {
    writeFileSync(fd, body, 'utf8');
    fsyncSync(fd);
  } finally {
    closeSync(fd);
  }
  try {
    const existing = lstatSync(target);
    if (existing.isSymbolicLink() || !existing.isFile()) throw fail(`${target} is not a regular private file`, 'private_state_permissions');
    requirePrivateMode(existing, 0o600, target);
  } catch (error) {
    if (error?.code !== 'ENOENT') {
      rmSync(temporary, { force: true });
      throw error;
    }
  }
  renameSync(temporary, target);
  const dfd = openSync(dir, 'r');
  try {
    fsyncSync(dfd);
  } finally {
    closeSync(dfd);
  }
}

function readJson(target, fallback) {
  try {
    return JSON.parse(readRegularNoFollow(target, true).toString('utf8'));
  } catch (error) {
    if (error?.code === 'ENOENT') return fallback;
    throw error;
  }
}

function writeFrame(value) {
  process.stdout.write(`${JSON.stringify(value)}\n`);
}

function stderr(value) {
  process.stderr.write(`${sanitizeExternalError(value, PROXY_URL)}\n`);
}

function sha256Bytes(value) {
  return createHash('sha256').update(value).digest('hex');
}

function safeFilename(value, fallback) {
  const name = path.basename(String(value || fallback)).replace(/[^A-Za-z0-9._-]/g, '_').slice(0, 180);
  return name || fallback;
}

function extensionForMime(mime, fallback = '.bin') {
  const table = new Map([
    ['image/jpeg', '.jpg'], ['image/png', '.png'], ['image/webp', '.webp'], ['image/gif', '.gif'],
    ['video/mp4', '.mp4'], ['video/quicktime', '.mov'], ['audio/ogg', '.ogg'], ['audio/opus', '.opus'],
    ['audio/mpeg', '.mp3'], ['audio/mp4', '.m4a'], ['application/pdf', '.pdf'], ['text/plain', '.txt'],
  ]);
  return table.get(String(mime || '').split(';', 1)[0].toLowerCase()) || fallback;
}

function unwrapMessage(message) {
  let current = message || {};
  for (let depth = 0; depth < 8; depth += 1) {
    const nested = current.ephemeralMessage?.message
      || current.viewOnceMessage?.message
      || current.viewOnceMessageV2?.message
      || current.viewOnceMessageV2Extension?.message
      || current.documentWithCaptionMessage?.message
      || current.editedMessage?.message;
    if (!nested) return current;
    current = nested;
  }
  return current;
}

function contextInfo(content) {
  for (const value of Object.values(content || {})) {
    if (value && typeof value === 'object' && value.contextInfo) return value.contextInfo;
  }
  return {};
}

function textFromContent(content) {
  const value = unwrapMessage(content);
  if (typeof value.conversation === 'string') return value.conversation;
  if (typeof value.extendedTextMessage?.text === 'string') return value.extendedTextMessage.text;
  if (typeof value.imageMessage?.caption === 'string') return value.imageMessage.caption;
  if (typeof value.videoMessage?.caption === 'string') return value.videoMessage.caption;
  if (typeof value.documentMessage?.caption === 'string') return value.documentMessage.caption;
  if (typeof value.buttonsResponseMessage?.selectedDisplayText === 'string') return value.buttonsResponseMessage.selectedDisplayText;
  if (typeof value.listResponseMessage?.title === 'string') return value.listResponseMessage.title;
  if (typeof value.templateButtonReplyMessage?.selectedDisplayText === 'string') return value.templateButtonReplyMessage.selectedDisplayText;
  if (typeof value.pollCreationMessage?.name === 'string') return value.pollCreationMessage.name;
  return '';
}

function normalizeKey(key) {
  const value = requiredObject(key, 'message key');
  return {
    id: requiredString(String(value.id || ''), 'message key.id'),
    remoteJid: normalizeJid(requiredString(String(value.remoteJid || ''), 'message key.remoteJid')),
    fromMe: Boolean(value.fromMe),
    participant: value.participant ? normalizeJid(String(value.participant)) : undefined,
  };
}

const SESSION_DIR = path.resolve(requiredString(process.env.OCTOPUS_WHATSAPP_BRIDGE_SESSION_DIR || '', 'OCTOPUS_WHATSAPP_BRIDGE_SESSION_DIR'));
const MEDIA_DIR = path.join(SESSION_DIR, 'media');
const EVENTS_DIR = path.join(SESSION_DIR, 'pending-events');
const EFFECTS_DIR = path.join(SESSION_DIR, 'effects');
const STATE_PATH = path.join(SESSION_DIR, 'bridge-state.json');
const MESSAGE_STORE_PATH = path.join(SESSION_DIR, 'message-store.json');
const MODE = process.env.OCTOPUS_WHATSAPP_BRIDGE_MODE || 'bot';
const MAX_DOWNLOAD_BYTES = envInteger('OCTOPUS_WHATSAPP_MAX_DOWNLOAD_BYTES', 134217728, 1, 1073741824);
const MAX_UPLOAD_BYTES = envInteger('OCTOPUS_WHATSAPP_MAX_UPLOAD_BYTES', 134217728, 1, 1073741824);
const SEND_TIMEOUT_MS = envInteger('OCTOPUS_WHATSAPP_SEND_TIMEOUT_MS', 60000, 1000, 600000);
const MAX_RETRIES = envInteger('OCTOPUS_WHATSAPP_MAX_RETRIES', 3, 0, 10);
const RETRY_BACKOFF_MS = envInteger('OCTOPUS_WHATSAPP_RETRY_BACKOFF_MS', 500, 10, 60000);
const RETRY_BACKOFF_CAP_MS = envInteger('OCTOPUS_WHATSAPP_RETRY_BACKOFF_CAP_MS', 5000, 10, 300000);
const RECONNECT_BACKOFF_MS = envInteger('OCTOPUS_WHATSAPP_RECONNECT_BACKOFF_MS', 1000, 10, 300000);
const RECONNECT_BACKOFF_CAP_MS = envInteger('OCTOPUS_WHATSAPP_RECONNECT_BACKOFF_CAP_MS', 60000, 10, 600000);
const EVENT_REPLAY_LIMIT = envInteger('OCTOPUS_WHATSAPP_EVENT_REPLAY_LIMIT', 10000, 100, 1000000);
const EFFECT_JOURNAL_LIMIT = envInteger('OCTOPUS_WHATSAPP_EFFECT_JOURNAL_LIMIT', 10000, 100, 1000000);
const PRIVACY_MODE_EPOCH_OFFSET_SECONDS = 77980457;
const ALLOWED_USERS = parseIdentifierSet(process.env.OCTOPUS_WHATSAPP_BRIDGE_ALLOWED_USERS || '');
const ALLOWED_GROUPS = new Set(String(process.env.OCTOPUS_WHATSAPP_BRIDGE_ALLOWED_GROUPS || '').split(',').map(value => normalizeJid(value)).filter(Boolean));
const ALLOW_ALL_USERS = ['1', 'true', 'yes', 'on'].includes(String(process.env.OCTOPUS_WHATSAPP_BRIDGE_ALLOW_ALL_USERS || '').toLowerCase());
const DENY_UNMATCHED = !['0', 'false', 'no', 'off'].includes(String(process.env.OCTOPUS_WHATSAPP_BRIDGE_DENY_UNMATCHED || 'true').toLowerCase());
const ALLOW_SELF_CHAT = ['1', 'true', 'yes', 'on'].includes(String(process.env.OCTOPUS_WHATSAPP_BRIDGE_ALLOW_SELF_CHAT || 'true').toLowerCase());
const PAIR_ON_BOOT = !['0', 'false', 'no', 'off'].includes(String(process.env.OCTOPUS_WHATSAPP_BRIDGE_PAIR_ON_BOOT || 'true').toLowerCase());
const ALLOW_BROADCASTS = ['1', 'true', 'yes', 'on'].includes(String(process.env.OCTOPUS_WHATSAPP_BRIDGE_ALLOW_BROADCASTS || 'false').toLowerCase());
const ALLOW_NEWSLETTERS = ['1', 'true', 'yes', 'on'].includes(String(process.env.OCTOPUS_WHATSAPP_BRIDGE_ALLOW_NEWSLETTERS || 'false').toLowerCase());
const PROXY_URL = validateProxyUrl(process.env.OCTOPUS_WHATSAPP_BRIDGE_PROXY || '');
const ON_DISCONNECT_PRESENCE = String(process.env.OCTOPUS_WHATSAPP_ON_DISCONNECT_PRESENCE || '').trim();

validatePrivateTree(SESSION_DIR);
ensurePrivateDirectory(MEDIA_DIR);
ensurePrivateDirectory(EVENTS_DIR);
ensurePrivateDirectory(EFFECTS_DIR);

let bridgeState = readJson(STATE_PATH, { schema_version: 1, next_event_seq: 1, seen_event_ids: [], recently_sent_ids: [] });
if (bridgeState?.schema_version !== 1 || !Number.isSafeInteger(bridgeState.next_event_seq) || !Array.isArray(bridgeState.seen_event_ids)) {
  throw fail('bridge-state.json is malformed', 'state_corrupt');
}
let seenEvents = new Set(bridgeState.seen_event_ids.map(String));
let recentlySentIds = new Set((bridgeState.recently_sent_ids || []).map(String));
let messageStore = readJson(MESSAGE_STORE_PATH, { schema_version: 1, messages: {} });
if (messageStore?.schema_version !== 1 || !messageStore.messages || typeof messageStore.messages !== 'object' || Array.isArray(messageStore.messages)) {
  throw fail('message-store.json is malformed', 'state_corrupt');
}

function persistBridgeState() {
  while (seenEvents.size > EVENT_REPLAY_LIMIT) seenEvents.delete(seenEvents.values().next().value);
  while (recentlySentIds.size > 5000) recentlySentIds.delete(recentlySentIds.values().next().value);
  bridgeState = { schema_version: 1, next_event_seq: bridgeState.next_event_seq, seen_event_ids: [...seenEvents], recently_sent_ids: [...recentlySentIds] };
  atomicJson(STATE_PATH, bridgeState);
}

function persistMessageStore() {
  const entries = Object.entries(messageStore.messages);
  if (entries.length > 5000) messageStore.messages = Object.fromEntries(entries.slice(entries.length - 5000));
  atomicJson(MESSAGE_STORE_PATH, messageStore);
}

function eventPath(seq) {
  return path.join(EVENTS_DIR, `${String(seq).padStart(20, '0')}.json`);
}

function queueEvent(eventId, event) {
  const id = requiredString(eventId, 'event id');
  if (seenEvents.has(id)) return null;
  const pendingCount = readdirSync(EVENTS_DIR).filter(name => /^[0-9]{20}\.json$/.test(name)).length;
  if (pendingCount >= EVENT_REPLAY_LIMIT) throw fail('pending event backlog reached its configured bound', 'event_backlog_full', { pending_count: pendingCount });
  const seq = bridgeState.next_event_seq;
  bridgeState.next_event_seq += 1;
  const envelope = { schema_version: 1, seq, event_id: id, event };
  if (existsSync(eventPath(seq))) throw fail('pending event sequence would overwrite durable evidence', 'state_corrupt', { seq });
  atomicJson(eventPath(seq), envelope);
  seenEvents.add(id);
  persistBridgeState();
  writeFrame({ type: 'event', ...envelope });
  return seq;
}

function replayPendingEvents() {
  const files = readdirSync(EVENTS_DIR).filter(name => /^[0-9]{20}\.json$/.test(name)).sort();
  if (files.length > EVENT_REPLAY_LIMIT) throw fail('pending event replay exceeds configured bound', 'state_corrupt');
  const envelopes = [];
  let maximumSeq = 0;
  for (const filename of files) {
    const envelope = readJson(path.join(EVENTS_DIR, filename), null);
    if (!envelope || envelope.schema_version !== 1 || !Number.isSafeInteger(envelope.seq) || typeof envelope.event_id !== 'string' || !envelope.event || typeof envelope.event !== 'object') {
      throw fail(`pending event ${filename} is malformed`, 'state_corrupt');
    }
    if (path.basename(eventPath(envelope.seq)) !== filename) throw fail(`pending event ${filename} sequence does not match its filename`, 'state_corrupt');
    maximumSeq = Math.max(maximumSeq, envelope.seq);
    seenEvents.add(envelope.event_id);
    envelopes.push(envelope);
  }
  bridgeState.next_event_seq = Math.max(bridgeState.next_event_seq, maximumSeq + 1);
  persistBridgeState();
  for (const envelope of envelopes) {
    writeFrame({ type: 'event', ...envelope });
  }
}

function acknowledgeEvent(seq) {
  requiredInteger(seq, 'event seq', 1);
  const target = eventPath(seq);
  if (existsSync(target)) durableRemove(target);
  return { acknowledged_seq: seq };
}

function effectPath(key) {
  const digest = sha256Bytes(Buffer.from(requiredString(key, 'idempotency key'), 'utf8'));
  return path.join(EFFECTS_DIR, `${digest}.json`);
}

function loadEffect(key) {
  const target = effectPath(key);
  return existsSync(target) ? readJson(target, null) : null;
}

function saveEffect(key, state) {
  atomicJson(effectPath(key), state);
}

function durableRemove(target) {
  if (!existsSync(target)) return;
  unlinkSync(target);
  const dfd = openSync(path.dirname(target), 'r');
  try {
    fsyncSync(dfd);
  } finally {
    closeSync(dfd);
  }
}

function pruneEffectJournal(targetCount) {
  const files = readdirSync(EFFECTS_DIR).filter(name => /^[0-9a-f]{64}\.json$/.test(name)).sort((a, b) => privateFileMtime(path.join(EFFECTS_DIR, a)) - privateFileMtime(path.join(EFFECTS_DIR, b)));
  let remaining = files.length;
  for (const filename of files) {
    if (remaining <= targetCount) break;
    const target = path.join(EFFECTS_DIR, filename);
    const state = readJson(target, null);
    const terminal = state?.status === 'succeeded' || state?.status === 'failed';
    const safelyRepeatable = terminal && IDEMPOTENT_EFFECTS.has(state?.operation);
    if (!state?.acknowledged && !safelyRepeatable) continue;
    durableRemove(target);
    remaining -= 1;
  }
  return remaining;
}

function ensureEffectCapacity(key) {
  const target = effectPath(key);
  if (existsSync(target)) return;
  const remaining = pruneEffectJournal(EFFECT_JOURNAL_LIMIT - 1);
  if (remaining >= EFFECT_JOURNAL_LIMIT) throw fail('effect journal reached its bound with unresolved effects', 'effect_journal_full', { limit: EFFECT_JOURNAL_LIMIT });
}

function acknowledgeEffect(key) {
  const current = loadEffect(key);
  if (!current) return { acknowledged: false, reason: 'not_found' };
  if (current.status !== 'succeeded' && current.status !== 'failed') throw fail('cannot acknowledge a non-terminal external effect', 'effect_not_terminal');
  saveEffect(key, { ...current, acknowledged: true, acknowledged_at_unix_ms: Date.now() });
  pruneEffectJournal(EFFECT_JOURNAL_LIMIT);
  return { acknowledged: true, status: current.status };
}

function messageIdentity(key) {
  return `${key.remoteJid}:${key.participant || ''}:${key.id}`;
}

function messageLookupKey(chatId, messageId) {
  return `${normalizeJid(chatId)}:${String(messageId || '')}`;
}

function rememberMessage(message) {
  if (!message?.key?.id || !message?.key?.remoteJid || !message.message) return;
  const key = normalizeKey(message.key);
  messageStore.messages[messageLookupKey(key.remoteJid, key.id)] = { key, message: message.message, messageTimestamp: Number(message.messageTimestamp || Date.now() / 1000) };
  persistMessageStore();
}

function quoteFor(messageId, chatId) {
  if (!messageId) return undefined;
  const stored = messageStore.messages[messageLookupKey(chatId, messageId)];
  if (!stored || normalizeJid(stored.key?.remoteJid) !== normalizeJid(chatId)) throw fail('quoted message is unavailable in this chat', 'quoted_message_unavailable');
  return stored;
}

function botIds() {
  return [...new Set([sock?.user?.id, sock?.user?.lid].map(normalizeJid).filter(Boolean))];
}

function isSelfChat(chatId) {
  const target = normalizeIdentifier(chatId);
  return botIds().some(value => normalizeIdentifier(value) === target);
}

function authorizedMessage(message) {
  const key = normalizeKey(message.key);
  const chatId = key.remoteJid;
  const isGroup = chatId.endsWith('@g.us');
  const senderId = key.participant || chatId;
  if (chatId.endsWith('@newsletter') && !ALLOW_NEWSLETTERS) return { allowed: false, reason: 'newsletter_refused' };
  if (chatId.endsWith('@broadcast') && !ALLOW_BROADCASTS) return { allowed: false, reason: 'broadcast_refused' };
  if (MODE === 'self-chat') {
    return { allowed: ALLOW_SELF_CHAT && key.fromMe && !recentlySentIds.has(key.id) && !isGroup && isSelfChat(chatId), reason: ALLOW_SELF_CHAT ? 'self_chat_only' : 'self_chat_disabled' };
  }
  if (key.fromMe) return { allowed: false, reason: 'outbound_echo' };
  if (isGroup && DENY_UNMATCHED && !ALLOWED_GROUPS.has('*') && !ALLOWED_GROUPS.has(chatId)) return { allowed: false, reason: 'group_allowlist' };
  if (DENY_UNMATCHED && !ALLOW_ALL_USERS && !matchesIdentifier(senderId, ALLOWED_USERS, SESSION_DIR)) return { allowed: false, reason: 'user_allowlist' };
  return { allowed: true, reason: null };
}

function queueBoundaryFailure(scope, raw, error) {
  if (error?.code === 'event_backlog_full' || error?.code === 'state_corrupt') throw error;
  const rawKey = raw?.key || raw?.message?.key || {};
  let key;
  try {
    key = normalizeKey(rawKey);
  } catch {
    const chatId = normalizeJid(raw?.id || rawKey?.remoteJid || '');
    key = { id: String(rawKey?.id || ''), remoteJid: chatId, fromMe: Boolean(rawKey?.fromMe), participant: normalizeJid(rawKey?.participant || '') || undefined };
  }
  const eventId = `message.failure:${scope}:${key.remoteJid || 'unknown'}:${key.id || sha256Bytes(Buffer.from(JSON.stringify(raw || {}), 'utf8')).slice(0, 24)}:${Date.now()}`;
  queueEvent(eventId, {
    event_type: 'message.failure',
    event_id: eventId,
    chat_id: key.remoteJid || null,
    message_id: key.id || null,
    sender_id: key.participant || key.remoteJid || null,
    participant_id: key.participant || null,
    from_me: Boolean(key.fromMe),
    is_group: Boolean(key.remoteJid?.endsWith('@g.us')),
    bot_ids: botIds(),
    failure: {
      scope,
      code: error?.code || 'boundary_failure',
      message: sanitizeExternalError(String(error?.message || error), PROXY_URL),
      details: sanitizeExternalError(error?.details || {}, PROXY_URL),
    },
    raw_event: raw || null,
  });
}

async function downloadOne(message, descriptor, category) {
  const declared = Number(descriptor.fileLength || 0);
  if (Number.isFinite(declared) && declared > MAX_DOWNLOAD_BYTES) throw fail('inbound media exceeds max_download_bytes', 'media_too_large', { declared });
  const mime = String(descriptor.mimetype || 'application/octet-stream');
  const base = safeFilename(descriptor.fileName, `${category}${extensionForMime(mime)}`);
  const staged = path.join(MEDIA_DIR, `.inbound-${process.pid}.${Date.now()}.${randomBytes(8).toString('hex')}.tmp`);
  const fd = openSync(staged, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | (constants.O_NOFOLLOW || 0), 0o600);
  const digestState = createHash('sha256');
  let observed = 0;
  let stream;
  try {
    stream = await downloadMediaMessage(message, 'stream', {}, { logger, reuploadRequest: sock.updateMediaMessage });
    if (!stream || typeof stream[Symbol.asyncIterator] !== 'function') throw fail('media transport did not return a byte stream', 'media_transport_invalid');
    for await (const value of stream) {
      const chunk = Buffer.isBuffer(value) ? value : Buffer.from(value);
      if (observed + chunk.length > MAX_DOWNLOAD_BYTES) {
        stream.destroy?.();
        throw fail('downloaded media exceeds max_download_bytes', 'media_too_large', { observed: observed + chunk.length });
      }
      writeFileSync(fd, chunk);
      digestState.update(chunk);
      observed += chunk.length;
    }
    fsyncSync(fd);
  } catch (error) {
    stream?.destroy?.();
    closeSync(fd);
    durableRemove(staged);
    throw error;
  }
  closeSync(fd);
  const digest = digestState.digest('hex');
  const filename = `${category}-${digest.slice(0, 24)}-${base}`;
  const target = path.join(MEDIA_DIR, filename);
  if (existsSync(target)) {
    const existing = digestRegularNoFollow(target, MAX_DOWNLOAD_BYTES, true);
    durableRemove(staged);
    if (existing.digest !== digest || existing.size !== observed) throw fail('cached inbound media evidence is inconsistent', 'private_state_corrupt');
  } else {
    try {
      linkSync(staged, target);
      durableRemove(staged);
    } catch (error) {
      if (error?.code !== 'EEXIST') {
        durableRemove(staged);
        throw error;
      }
      const existing = digestRegularNoFollow(target, MAX_DOWNLOAD_BYTES, true);
      durableRemove(staged);
      if (existing.digest !== digest || existing.size !== observed) throw fail('cached inbound media appeared with inconsistent evidence', 'private_state_corrupt');
    }
  }
  return { payload_path: target, sha256: digest, size: observed, mime_type: mime, filename: base };
}

async function normalizeInboundMessage(message, eventType = 'message.create') {
  const key = normalizeKey(message.key);
  const content = unwrapMessage(message.message);
  const context = contextInfo(content);
  const senderId = key.participant || key.remoteJid;
  const group = key.remoteJid.endsWith('@g.us');
  const event = {
    event_type: eventType,
    event_id: `${eventType}:${messageIdentity(key)}:${String(message.messageTimestamp || '')}`,
    message_id: key.id,
    chat_id: key.remoteJid,
    sender_id: senderId,
    sender_name: message.pushName || normalizeIdentifier(senderId),
    from_me: key.fromMe,
    is_group: group,
    timestamp: Number(message.messageTimestamp || 0),
    text: textFromContent(content),
    mentioned_ids: [...new Set((context.mentionedJid || []).map(normalizeJid).filter(Boolean))],
    bot_ids: botIds(),
    quoted: null,
    media: [],
    location: null,
    contacts: [],
    poll: null,
    interactive: null,
    raw_message_keys: Object.keys(content),
    raw_message: { key: message.key, messageTimestamp: message.messageTimestamp, pushName: message.pushName || null, message: content },
  };
  if (group) {
    let metadata = groupMetadataCache.get(key.remoteJid);
    if (!metadata) {
      try {
        metadata = await sock.groupMetadata(key.remoteJid);
        groupMetadataCache.set(key.remoteJid, metadata);
      } catch {}
    }
    if (metadata) {
      event.chat_name = metadata.subject || null;
      event.group = { owner: metadata.owner || null, description: metadata.desc || null, announce: Boolean(metadata.announce), restrict: Boolean(metadata.restrict), participant_count: (metadata.participants || []).length };
    }
  }
  if (context.stanzaId) {
    event.quoted = {
      message_id: String(context.stanzaId),
      participant: context.participant ? normalizeJid(context.participant) : null,
      remote_jid: context.remoteJid ? normalizeJid(context.remoteJid) : null,
      text: textFromContent(context.quotedMessage || ''),
    };
  }
  const candidates = [
    ['image', content.imageMessage], ['video', content.videoMessage], ['audio', content.audioMessage],
    ['document', content.documentMessage], ['sticker', content.stickerMessage],
  ];
  for (const [category, descriptor] of candidates) {
    if (!descriptor) continue;
    try {
      event.media.push({ category, ...await downloadOne(message, descriptor, category), seconds: Number(descriptor.seconds || 0), width: Number(descriptor.width || 0), height: Number(descriptor.height || 0), ptt: Boolean(descriptor.ptt), animated: Boolean(descriptor.isAnimated) });
    } catch (error) {
      event.media.push({ category, failure: { code: error.code || 'media_download_failed', message: sanitizeExternalError(error.message, PROXY_URL), details: sanitizeExternalError(error.details || {}, PROXY_URL) }, mime_type: String(descriptor.mimetype || 'application/octet-stream'), filename: descriptor.fileName ? safeFilename(descriptor.fileName, 'file') : null });
    }
  }
  const location = content.locationMessage || content.liveLocationMessage;
  if (location) {
    event.location = { latitude: Number(location.degreesLatitude), longitude: Number(location.degreesLongitude), name: location.name || null, address: location.address || null, url: location.url || null, live: Boolean(content.liveLocationMessage), accuracy_meters: Number(location.accuracyInMeters || 0) };
  }
  const contact = content.contactMessage;
  if (contact) event.contacts.push({ display_name: contact.displayName || null, vcard: contact.vcard || '' });
  for (const item of content.contactsArrayMessage?.contacts || []) event.contacts.push({ display_name: item.displayName || null, vcard: item.vcard || '' });
  const poll = content.pollCreationMessage || content.pollCreationMessageV2 || content.pollCreationMessageV3;
  if (poll) event.poll = { name: poll.name || '', options: (poll.options || []).map(item => item.optionName || ''), selectable_count: Number(poll.selectableOptionsCount || 1) };
  if (content.buttonsResponseMessage) event.interactive = { type: 'button_response', id: content.buttonsResponseMessage.selectedButtonId || null, text: content.buttonsResponseMessage.selectedDisplayText || null };
  if (content.listResponseMessage) event.interactive = { type: 'list_response', id: content.listResponseMessage.singleSelectReply?.selectedRowId || null, title: content.listResponseMessage.title || null, description: content.listResponseMessage.description || null };
  if (content.templateButtonReplyMessage) event.interactive = { type: 'template_response', id: content.templateButtonReplyMessage.selectedId || null, text: content.templateButtonReplyMessage.selectedDisplayText || null, index: Number(content.templateButtonReplyMessage.selectedIndex || 0) };
  if (content.interactiveResponseMessage?.nativeFlowResponseMessage) {
    const response = content.interactiveResponseMessage.nativeFlowResponseMessage;
    let params = null;
    try {
      params = response.paramsJson ? JSON.parse(response.paramsJson) : null;
    } catch {
      params = { raw: response.paramsJson || '' };
    }
    event.interactive = { type: 'native_flow_response', name: response.name || null, params };
  }
  return event;
}

async function boundedCall(action, timeoutMs = SEND_TIMEOUT_MS) {
  let timer;
  try {
    return await Promise.race([
      action(),
      new Promise((_, reject) => { timer = setTimeout(() => reject(fail(`WhatsApp operation timed out after ${timeoutMs}ms`, 'operation_timeout')), timeoutMs); }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

function retryableError(error) {
  const code = Number(new Boom(error)?.output?.statusCode || error?.output?.statusCode || 0);
  return code === 408 || code === 429 || code >= 500;
}

async function retryIdempotent(action) {
  let backoff = RETRY_BACKOFF_MS;
  let last;
  for (let attempt = 0; attempt <= MAX_RETRIES; attempt += 1) {
    try {
      return await boundedCall(action);
    } catch (error) {
      last = error;
      if (attempt >= MAX_RETRIES || !retryableError(error)) throw error;
      await new Promise(resolve => setTimeout(resolve, backoff));
      backoff = Math.min(backoff * 2, RETRY_BACKOFF_CAP_MS);
    }
  }
  throw last;
}

function splitText(text, maximum) {
  const value = String(text || '');
  if (!value) return [];
  const chunks = [];
  let rest = value;
  while (rest.length > maximum) {
    let split = rest.lastIndexOf('\n', maximum);
    if (split < Math.floor(maximum / 2)) split = rest.lastIndexOf(' ', maximum);
    if (split < 1) split = maximum;
    let chunk = rest.slice(0, split).trimEnd();
    rest = rest.slice(split).trimStart();
    const fences = (chunk.match(/```/g) || []).length;
    if (fences % 2 === 1) {
      chunk += '\n```';
      rest = `\`\`\`\n${rest}`;
    }
    chunks.push(chunk);
  }
  if (rest) chunks.push(rest);
  return chunks;
}

function checkFile(payload) {
  const target = path.resolve(requiredString(payload.payload_path, 'payload_path'));
  const fd = openRegularNoFollow(target, false);
  let info;
  let content;
  try {
    info = fstatSync(fd);
    if (info.size > MAX_UPLOAD_BYTES) throw fail('outbound media exceeds max_upload_bytes', 'media_too_large', { size: info.size });
    content = readBoundedFd(fd, MAX_UPLOAD_BYTES, 'outbound media');
  } finally {
    closeSync(fd);
  }
  requiredInteger(payload.size, 'media size', 0, MAX_UPLOAD_BYTES);
  if (info.size !== payload.size || content.length !== payload.size) throw fail('outbound media size changed', 'media_integrity_mismatch');
  const digest = sha256Bytes(content);
  if (digest !== requiredString(payload.sha256, 'media sha256')) throw fail('outbound media hash mismatch', 'media_integrity_mismatch');
  return { content, target, digest };
}

let sock = null;
let connectionState = 'disconnected';
let reconnectBackoff = RECONNECT_BACKOFF_MS;
let reconnectTimer = null;
let socketGeneration = 0;
let closing = false;
const groupMetadataCache = new Map();
const logger = pino(
  { level: process.env.OCTOPUS_WHATSAPP_DEBUG ? 'debug' : 'warn' },
  { write: value => process.stderr.write(String(sanitizeExternalError(String(value), PROXY_URL))) },
);
const proxyAgent = PROXY_URL ? new HttpsProxyAgent(PROXY_URL) : undefined;

function statusFrame(status, details = {}) {
  connectionState = status;
  writeFrame({ type: 'status', status, details: sanitizeExternalError(details, PROXY_URL), at_unix_ms: Date.now() });
}

function scheduleReconnect(reason) {
  if (closing || reconnectTimer) return;
  const delay = reconnectBackoff;
  reconnectBackoff = Math.min(reconnectBackoff * 2, RECONNECT_BACKOFF_CAP_MS);
  statusFrame('reconnecting', { reason: sanitizeExternalError(String(reason || 'unknown'), PROXY_URL), delay_ms: delay });
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    startSocket().catch(error => scheduleReconnect(error.message));
  }, delay);
}

async function startSocket() {
  const generation = ++socketGeneration;
  const { state, saveCreds } = await useMultiFileAuthState(SESSION_DIR);
  const { version } = await fetchLatestBaileysVersion();
  const current = makeWASocket({
    version,
    auth: state,
    logger,
    printQRInTerminal: false,
    browser: ['Octopus WhatsApp Interface', 'Chrome', '1.0'],
    syncFullHistory: false,
    markOnlineOnConnect: false,
    agent: proxyAgent,
    fetchAgent: proxyAgent,
    getMessage: async key => messageStore.messages[messageLookupKey(key?.remoteJid, key?.id)]?.message,
  });
  sock = current;
  statusFrame('connecting');
  current.ev.on('creds.update', async () => {
    await saveCreds();
  });
  current.ev.on('connection.update', update => {
    if (generation !== socketGeneration) return;
    if (update.qr) {
      if (PAIR_ON_BOOT) {
        writeFrame({ type: 'pairing', status: 'qr', qr: update.qr, at_unix_ms: Date.now() });
      } else {
        writeFrame({ type: 'pairing', status: 'required', at_unix_ms: Date.now() });
      }
    }
    if (update.connection === 'open') {
      reconnectBackoff = RECONNECT_BACKOFF_MS;
      statusFrame('connected', { user_ids: botIds() });
      const presence = process.env.OCTOPUS_WHATSAPP_ON_CONNECT_PRESENCE || '';
      if (presence) current.sendPresenceUpdate(presence).catch(() => {});
    }
    if (update.connection === 'close') {
      const reason = new Boom(update.lastDisconnect?.error)?.output?.statusCode;
      statusFrame(reason === DisconnectReason.loggedOut ? 'logged_out' : 'disconnected', { reason });
      if (reason === DisconnectReason.loggedOut) return;
      scheduleReconnect(reason);
    }
  });
  current.ev.on('messages.upsert', async ({ messages, type }) => {
    if (!['notify', 'append'].includes(type)) return;
    for (const message of messages) {
      try {
        if (!message?.message || !message?.key?.id) continue;
        rememberMessage(message);
        const gate = authorizedMessage(message);
        if (!gate.allowed) continue;
        const event = await normalizeInboundMessage(message);
        queueEvent(event.event_id, event);
      } catch (error) {
        queueBoundaryFailure('messages.upsert', message, error);
      }
    }
  });
  current.ev.on('messages.update', async updates => {
    for (const update of updates || []) {
      try {
        const key = normalizeKey(update.key);
        if (key.fromMe && (MODE === 'bot' || recentlySentIds.has(key.id))) continue;
        if (Array.isArray(update.update?.pollUpdates) && update.update.pollUpdates.length) {
          const stored = messageStore.messages[messageLookupKey(key.remoteJid, key.id)];
          let aggregateVotes = [];
          if (stored?.message) {
            try {
              aggregateVotes = getAggregateVotesInPollMessage({ message: stored.message, pollUpdates: update.update.pollUpdates });
            } catch {}
          }
          const event = { event_type: 'poll.update', event_id: `poll.update:${messageIdentity(key)}:${sha256Bytes(Buffer.from(JSON.stringify(update.update.pollUpdates), 'utf8')).slice(0, 24)}`, message_id: key.id, chat_id: key.remoteJid, participant_id: key.participant || null, sender_id: key.participant || key.remoteJid, from_me: key.fromMe, is_group: key.remoteJid.endsWith('@g.us'), poll_updates: update.update.pollUpdates, aggregate_votes: aggregateVotes, raw_event: update };
          queueEvent(event.event_id, event);
        }
        const protocol = unwrapMessage(update.update?.message || {}).protocolMessage;
        if (protocol?.type === 0 || protocol?.type === 'REVOKE') {
          const deleted = normalizeKey(protocol.key || key);
          const event = { event_type: 'message.delete', event_id: `message.delete:${messageIdentity(deleted)}`, message_id: deleted.id, chat_id: deleted.remoteJid, participant_id: deleted.participant || null, from_me: deleted.fromMe, is_group: deleted.remoteJid.endsWith('@g.us'), raw_event: update };
          queueEvent(event.event_id, event);
        } else if (update.update?.message) {
          const message = { key, message: update.update.message, messageTimestamp: Math.floor(Date.now() / 1000) };
          rememberMessage(message);
          const event = await normalizeInboundMessage(message, 'message.edit');
          event.event_id = `message.edit:${messageIdentity(key)}:${sha256Bytes(Buffer.from(JSON.stringify(update.update.message), 'utf8')).slice(0, 24)}`;
          queueEvent(event.event_id, event);
        }
      } catch (error) {
        queueBoundaryFailure('messages.update', update, error);
      }
    }
  });
  current.ev.on('messages.delete', value => {
    const keys = value?.keys || [];
    for (const raw of keys) {
      try {
        const key = normalizeKey(raw);
        if (key.fromMe && (MODE === 'bot' || recentlySentIds.has(key.id))) continue;
        const event = { event_type: 'message.delete', event_id: `message.delete:${messageIdentity(key)}`, message_id: key.id, chat_id: key.remoteJid, participant_id: key.participant || null, from_me: key.fromMe, is_group: key.remoteJid.endsWith('@g.us'), raw_event: value };
        queueEvent(event.event_id, event);
      } catch (error) {
        queueBoundaryFailure('messages.delete', { key: raw, value }, error);
      }
    }
  });
  current.ev.on('messages.reaction', values => {
    for (const value of values || []) {
      try {
        const key = normalizeKey(value.key);
        const reaction = value.reaction || {};
        const reactionParticipant = normalizeJid(reaction.key?.participant || '');
        if (reaction.key?.fromMe || botIds().includes(reactionParticipant)) continue;
        const event = { event_type: 'reaction.update', event_id: `reaction:${messageIdentity(key)}:${reaction.key?.participant || ''}:${reaction.text || ''}:${reaction.senderTimestampMs || Date.now()}`, message_id: key.id, chat_id: key.remoteJid, participant_id: reactionParticipant, sender_id: reactionParticipant, is_group: key.remoteJid.endsWith('@g.us'), emoji: reaction.text || '', removed: !reaction.text, timestamp_ms: Number(reaction.senderTimestampMs || Date.now()), raw_event: value };
        queueEvent(event.event_id, event);
      } catch (error) {
        queueBoundaryFailure('messages.reaction', value, error);
      }
    }
  });
  current.ev.on('message-receipt.update', values => {
    for (const value of values || []) {
      try {
        const key = normalizeKey(value.key);
        const receipt = value.receipt || {};
        const event = { event_type: 'receipt.update', event_id: `receipt:${messageIdentity(key)}:${receipt.userJid || ''}:${receipt.readTimestamp || receipt.receiptTimestamp || Date.now()}`, message_id: key.id, chat_id: key.remoteJid, participant_id: normalizeJid(receipt.userJid || ''), sender_id: normalizeJid(receipt.userJid || ''), is_group: key.remoteJid.endsWith('@g.us'), read_timestamp: Number(receipt.readTimestamp || 0), delivered_timestamp: Number(receipt.receiptTimestamp || 0), played_timestamp: Number(receipt.playedTimestamp || 0), raw_event: value };
        queueEvent(event.event_id, event);
      } catch (error) {
        queueBoundaryFailure('message-receipt.update', value, error);
      }
    }
  });
  current.ev.on('presence.update', value => {
    try {
      const chatId = normalizeJid(value.id || '');
      if (!chatId) return;
      for (const [participant, presence] of Object.entries(value.presences || {})) {
        const participantId = normalizeJid(participant);
        const event = { event_type: 'presence.update', event_id: `presence:${chatId}:${participantId}:${presence?.lastKnownPresence || ''}:${Date.now()}`, chat_id: chatId, is_group: chatId.endsWith('@g.us'), participant_id: participantId, sender_id: participantId, presence: presence || {}, raw_event: value };
        queueEvent(event.event_id, event);
      }
    } catch (error) {
      queueBoundaryFailure('presence.update', value, error);
    }
  });
  current.ev.on('groups.upsert', groups => {
    for (const group of groups || []) {
      try {
        if (!group?.id) continue;
        const chatId = normalizeJid(group.id);
        groupMetadataCache.set(chatId, group);
        const event = { event_type: 'group.update', event_id: `group.upsert:${chatId}:${sha256Bytes(Buffer.from(JSON.stringify(group), 'utf8')).slice(0, 24)}`, chat_id: chatId, is_group: true, system_event: true, change: 'upsert', group, raw_event: group };
        queueEvent(event.event_id, event);
      } catch (error) {
        queueBoundaryFailure('groups.upsert', group, error);
      }
    }
  });
  current.ev.on('groups.update', groups => {
    for (const update of groups || []) {
      try {
        if (!update?.id) continue;
        const chatId = normalizeJid(update.id);
        groupMetadataCache.set(chatId, { ...(groupMetadataCache.get(chatId) || {}), ...update });
        const event = { event_type: 'group.update', event_id: `group.update:${chatId}:${sha256Bytes(Buffer.from(JSON.stringify(update), 'utf8')).slice(0, 24)}`, chat_id: chatId, is_group: true, system_event: true, change: 'update', group: update, raw_event: update };
        queueEvent(event.event_id, event);
      } catch (error) {
        queueBoundaryFailure('groups.update', update, error);
      }
    }
  });
  current.ev.on('group-participants.update', update => {
    try {
      const chatId = normalizeJid(update?.id || '');
      if (!chatId) throw fail('group participant event has no chat id', 'boundary_rejected');
      groupMetadataCache.delete(chatId);
      const event = { event_type: 'group.participants.update', event_id: `group.participants.update:${chatId}:${update.action || ''}:${sha256Bytes(Buffer.from(JSON.stringify(update), 'utf8')).slice(0, 24)}`, chat_id: chatId, is_group: true, system_event: true, action: update.action || null, participants: (update.participants || []).map(normalizeJid), author: normalizeJid(update.author || ''), raw_event: update };
      queueEvent(event.event_id, event);
    } catch (error) {
      queueBoundaryFailure('group-participants.update', update, error);
    }
  });
}

async function sendMessagePayload(chatId, payload, options = {}) {
  if (!sock || connectionState !== 'connected') throw fail('WhatsApp is not connected', 'not_connected');
  const sent = await boundedCall(() => sock.sendMessage(chatId, payload, options));
  if (!sent?.key?.id) throw fail('WhatsApp send returned no message id', 'invalid_provider_result');
  recentlySentIds.add(sent.key.id);
  persistBridgeState();
  rememberMessage(sent);
  return { message_id: sent.key.id, chat_id: sent.key.remoteJid || chatId, timestamp: Number(sent.messageTimestamp || 0) };
}

async function sendNativeFlow(chatId, payload, buttons, quoted) {
  if (!sock || connectionState !== 'connected') throw fail('WhatsApp is not connected', 'not_connected');
  const interactiveMessage = proto.Message.InteractiveMessage.create({
    body: proto.Message.InteractiveMessage.Body.create({ text: requiredString(payload.text, 'text') }),
    footer: payload.footer ? proto.Message.InteractiveMessage.Footer.create({ text: requiredString(payload.footer, 'footer') }) : undefined,
    header: payload.title ? proto.Message.InteractiveMessage.Header.create({ title: requiredString(payload.title, 'title'), hasMediaAttachment: false }) : undefined,
    nativeFlowMessage: proto.Message.InteractiveMessage.NativeFlowMessage.create({ buttons, messageParamsJson: '{}', messageVersion: 1 }),
  });
  const generated = generateWAMessageFromContent(chatId, proto.Message.create({ interactiveMessage }), { userJid: requiredString(sock.user?.id, 'connected WhatsApp user id'), quoted });
  const businessNode = {
    tag: 'biz',
    attrs: {
      actual_actors: '2',
      host_storage: '2',
      privacy_mode_ts: String(Math.floor(Date.now() / 1000) - PRIVACY_MODE_EPOCH_OFFSET_SECONDS),
    },
    content: [
      { tag: 'interactive', attrs: { type: 'native_flow', v: '1' }, content: [{ tag: 'native_flow', attrs: { v: '9', name: 'mixed' } }] },
      { tag: 'quality_control', attrs: { source_type: 'third_party' } },
    ],
  };
  const additionalNodes = chatId.endsWith('@g.us') ? [businessNode] : [{ tag: 'bot', attrs: { biz_bot: '1' } }, businessNode];
  await boundedCall(() => sock.relayMessage(chatId, generated.message, { messageId: generated.key.id, additionalNodes }));
  if (!generated.key?.id) throw fail('WhatsApp relay returned no message id', 'invalid_provider_result');
  recentlySentIds.add(generated.key.id);
  persistBridgeState();
  rememberMessage(generated);
  return { message_id: generated.key.id, chat_id: generated.key.remoteJid || chatId, timestamp: Number(generated.messageTimestamp || 0) };
}

async function requireCommunityMetadata(chatId) {
  if (!chatId.endsWith('@g.us')) throw fail('community operation requires a community JID', 'boundary_rejected');
  const metadata = await retryIdempotent(() => sock.communityMetadata(chatId));
  if (!metadata?.isCommunity) throw fail('bound chat is not a community', 'boundary_rejected');
  return metadata;
}

async function executeOperation(operation, payload) {
  const chatId = normalizeJid(requiredString(payload.chat_id, 'chat_id'));
  if (operation === 'message.send') {
    const chunks = splitText(requiredString(payload.text, 'text'), requiredInteger(payload.max_chars || 4096, 'max_chars', 256, 65536));
    const ids = [];
    const quoted = payload.reply_to_message_id ? quoteFor(payload.reply_to_message_id, chatId) : undefined;
    for (let index = 0; index < chunks.length; index += 1) {
      const result = await sendMessagePayload(chatId, { text: chunks[index] }, index === 0 && quoted ? { quoted } : {});
      ids.push(result.message_id);
      if (index + 1 < chunks.length) await new Promise(resolve => setTimeout(resolve, requiredInteger(payload.chunk_delay_ms || 300, 'chunk_delay_ms', 0, 10000)));
    }
    return { chat_id: chatId, message_ids: ids, message_id: ids.at(-1) || null };
  }
  if (operation === 'message.edit') {
    const id = requiredString(payload.message_id, 'message_id');
    const key = { id, remoteJid: chatId, fromMe: true };
    const chunks = splitText(requiredString(payload.text, 'text'), requiredInteger(payload.max_chars || 4096, 'max_chars', 256, 65536));
    const first = await sendMessagePayload(chatId, { text: chunks[0], edit: key });
    const messageIds = [];
    for (let index = 1; index < chunks.length; index += 1) {
      const sent = await sendMessagePayload(chatId, { text: chunks[index] });
      messageIds.push(sent.message_id);
      if (index + 1 < chunks.length) await new Promise(resolve => setTimeout(resolve, requiredInteger(payload.chunk_delay_ms || 300, 'chunk_delay_ms', 0, 10000)));
    }
    return { ...first, edited_message_id: id, message_ids: messageIds };
  }
  if (operation === 'message.delete') {
    const id = requiredString(payload.message_id, 'message_id');
    await retryIdempotent(() => sock.sendMessage(chatId, { delete: { id, remoteJid: chatId, fromMe: Boolean(payload.from_me ?? true), participant: payload.participant_id ? normalizeJid(payload.participant_id) : undefined } }));
    return { chat_id: chatId, deleted_message_id: id };
  }
  if (operation === 'reaction.set' || operation === 'reaction.clear') {
    const id = requiredString(payload.message_id, 'message_id');
    const text = operation === 'reaction.clear' ? '' : requiredString(payload.emoji, 'emoji');
    const result = await sendMessagePayload(chatId, { react: { text, key: { id, remoteJid: chatId, fromMe: Boolean(payload.from_me ?? false), participant: payload.participant_id ? normalizeJid(payload.participant_id) : undefined } } });
    return { ...result, reacted_message_id: id, emoji: text };
  }
  if (operation === 'receipt.read') {
    const keys = (payload.message_keys || []).map(normalizeKey);
    if (!keys.length) throw fail('message_keys cannot be empty', 'boundary_rejected');
    await retryIdempotent(() => sock.readMessages(keys));
    return { chat_id: chatId, read_message_ids: keys.map(key => key.id) };
  }
  if (operation === 'typing.start' || operation === 'typing.stop') {
    const state = operation === 'typing.start' ? 'composing' : 'paused';
    await retryIdempotent(() => sock.sendPresenceUpdate(state, chatId));
    return { chat_id: chatId, presence: state };
  }
  if (operation === 'presence.set') {
    const state = requiredString(payload.presence, 'presence');
    if (!['available', 'unavailable', 'composing', 'recording', 'paused'].includes(state)) throw fail('presence is unsupported', 'boundary_rejected');
    await retryIdempotent(() => sock.sendPresenceUpdate(state, payload.chat_id ? chatId : undefined));
    return { chat_id: payload.chat_id ? chatId : null, presence: state };
  }
  if (operation === 'media.send' || operation === 'sticker.send') {
    let { content, target } = checkFile(payload);
    const mediaType = operation === 'sticker.send' ? 'sticker' : requiredString(payload.media_type, 'media_type');
    const mime = requiredString(payload.mime_type || 'application/octet-stream', 'mime_type');
    const caption = typeof payload.caption === 'string' && payload.caption ? payload.caption : undefined;
    let body;
    if (mediaType === 'image') body = { image: content, caption, mimetype: mime };
    else if (mediaType === 'video') body = { video: content, caption, mimetype: mime, gifPlayback: Boolean(payload.gif_playback) };
    else if (mediaType === 'audio') {
      let outputMime = mime;
      if (payload.ptt && !['audio/ogg', 'audio/opus', 'audio/ogg; codecs=opus'].includes(mime.toLowerCase())) {
        const converted = path.join(MEDIA_DIR, `.voice-${randomBytes(12).toString('hex')}.ogg`);
        try {
          execFileSync('ffmpeg', ['-y', '-i', target, '-ar', '48000', '-ac', '1', '-c:a', 'libopus', converted], { timeout: 30000, stdio: 'pipe' });
          content = readRegularNoFollow(converted, true, MAX_UPLOAD_BYTES);
          outputMime = 'audio/ogg; codecs=opus';
        } finally {
          rmSync(converted, { force: true });
        }
      }
      body = { audio: content, mimetype: outputMime, ptt: Boolean(payload.ptt) };
    }
    else if (mediaType === 'sticker') {
      if (mime.toLowerCase() !== 'image/webp') throw fail('WhatsApp stickers require image/webp bytes', 'boundary_rejected');
      body = { sticker: content, mimetype: mime };
    }
    else if (mediaType === 'document') body = { document: content, caption, mimetype: mime, fileName: safeFilename(payload.filename, 'document') };
    else throw fail('media_type is unsupported', 'boundary_rejected');
    const quoted = payload.reply_to_message_id ? quoteFor(payload.reply_to_message_id, chatId) : undefined;
    return sendMessagePayload(chatId, body, quoted ? { quoted } : {});
  }
  if (operation === 'location.send') {
    const latitude = Number(payload.latitude);
    const longitude = Number(payload.longitude);
    if (!Number.isFinite(latitude) || latitude < -90 || latitude > 90 || !Number.isFinite(longitude) || longitude < -180 || longitude > 180) throw fail('location coordinates are invalid', 'boundary_rejected');
    return sendMessagePayload(chatId, { location: { degreesLatitude: latitude, degreesLongitude: longitude, name: payload.name || undefined, address: payload.address || undefined } });
  }
  if (operation === 'contact.send') {
    const contacts = Array.isArray(payload.contacts) ? payload.contacts : [];
    if (!contacts.length || contacts.length > 100) throw fail('contacts must contain 1..100 entries', 'boundary_rejected');
    const normalized = contacts.map((item, index) => ({ displayName: requiredString(requiredObject(item, `contacts[${index}]`).display_name, `contacts[${index}].display_name`), vcard: requiredString(item.vcard, `contacts[${index}].vcard`) }));
    return sendMessagePayload(chatId, normalized.length === 1 ? { contacts: { displayName: normalized[0].displayName, contacts: normalized } } : { contacts: { displayName: `${normalized.length} contacts`, contacts: normalized } });
  }
  if (operation === 'poll.send') {
    const question = requiredString(payload.question, 'question');
    const options = Array.isArray(payload.options) ? payload.options.map((item, index) => requiredString(item, `options[${index}]`)) : [];
    if (options.length < 2 || options.length > 12 || new Set(options).size !== options.length) throw fail('poll options must contain 2..12 unique values', 'boundary_rejected');
    const selectableCount = requiredInteger(payload.selectable_count ?? 1, 'selectable_count', 1, options.length);
    const quoted = payload.reply_to_message_id ? quoteFor(payload.reply_to_message_id, chatId) : undefined;
    return sendMessagePayload(chatId, { poll: { name: question, values: options, selectableCount } }, quoted ? { quoted } : {});
  }
  if (operation === 'interactive.buttons.send') {
    const buttons = Array.isArray(payload.buttons) ? payload.buttons : [];
    if (buttons.length < 1 || buttons.length > 3) throw fail('buttons must contain 1..3 entries', 'boundary_rejected');
    const ids = new Set();
    const normalized = buttons.map((item, index) => {
      const value = requiredObject(item, `buttons[${index}]`);
      const id = requiredString(value.id, `buttons[${index}].id`);
      if (ids.has(id)) throw fail('button ids must be unique', 'boundary_rejected');
      ids.add(id);
      return proto.Message.InteractiveMessage.NativeFlowMessage.NativeFlowButton.create({
        name: 'quick_reply',
        buttonParamsJson: JSON.stringify({ display_text: requiredString(value.text, `buttons[${index}].text`), id }),
      });
    });
    const quoted = payload.reply_to_message_id ? quoteFor(payload.reply_to_message_id, chatId) : undefined;
    return sendNativeFlow(chatId, payload, normalized, quoted);
  }
  if (operation === 'interactive.list.send') {
    const sections = Array.isArray(payload.sections) ? payload.sections : [];
    if (sections.length < 1 || sections.length > 10) throw fail('sections must contain 1..10 entries', 'boundary_rejected');
    const ids = new Set();
    const normalizedSections = sections.map((section, sectionIndex) => {
      const value = requiredObject(section, `sections[${sectionIndex}]`);
      const rows = Array.isArray(value.rows) ? value.rows : [];
      if (rows.length < 1 || rows.length > 10) throw fail(`sections[${sectionIndex}].rows must contain 1..10 entries`, 'boundary_rejected');
      return { title: value.title === undefined ? undefined : requiredString(value.title, `sections[${sectionIndex}].title`, true), rows: rows.map((row, rowIndex) => {
        const item = requiredObject(row, `sections[${sectionIndex}].rows[${rowIndex}]`);
        const id = requiredString(item.id, `sections[${sectionIndex}].rows[${rowIndex}].id`);
        if (ids.has(id)) throw fail('list row ids must be unique', 'boundary_rejected');
        ids.add(id);
        return { id, title: requiredString(item.title, `sections[${sectionIndex}].rows[${rowIndex}].title`), description: item.description === undefined ? undefined : requiredString(item.description, `sections[${sectionIndex}].rows[${rowIndex}].description`, true) };
      }) };
    });
    const button = proto.Message.InteractiveMessage.NativeFlowMessage.NativeFlowButton.create({
      name: 'single_select',
      buttonParamsJson: JSON.stringify({ title: requiredString(payload.button_text, 'button_text'), sections: normalizedSections }),
    });
    const quoted = payload.reply_to_message_id ? quoteFor(payload.reply_to_message_id, chatId) : undefined;
    return sendNativeFlow(chatId, payload, [button], quoted);
  }
  if (operation === 'interactive.template.send') {
    const buttons = Array.isArray(payload.buttons) ? payload.buttons : [];
    if (buttons.length < 1 || buttons.length > 3) throw fail('template buttons must contain 1..3 entries', 'boundary_rejected');
    const quickReplyIds = new Set();
    const nativeButtons = buttons.map((button, offset) => {
      const value = requiredObject(button, `buttons[${offset}]`);
      const displayText = requiredString(value.text, `buttons[${offset}].text`);
      if (value.type === 'quick_reply') {
        const id = requiredString(value.id, `buttons[${offset}].id`);
        if (quickReplyIds.has(id)) throw fail('template quick-reply ids must be unique', 'boundary_rejected');
        quickReplyIds.add(id);
        return proto.Message.InteractiveMessage.NativeFlowMessage.NativeFlowButton.create({ name: 'quick_reply', buttonParamsJson: JSON.stringify({ display_text: displayText, id }) });
      }
      if (value.type === 'url') {
        const url = requiredString(value.url, `buttons[${offset}].url`);
        let parsed;
        try {
          parsed = new URL(url);
        } catch {
          throw fail(`buttons[${offset}].url is malformed`, 'boundary_rejected');
        }
        if (!['http:', 'https:'].includes(parsed.protocol)) throw fail(`buttons[${offset}].url requires http or https`, 'boundary_rejected');
        return proto.Message.InteractiveMessage.NativeFlowMessage.NativeFlowButton.create({ name: 'cta_url', buttonParamsJson: JSON.stringify({ display_text: displayText, url, merchant_url: url }) });
      }
      if (value.type === 'call') return proto.Message.InteractiveMessage.NativeFlowMessage.NativeFlowButton.create({ name: 'cta_call', buttonParamsJson: JSON.stringify({ display_text: displayText, phone_number: requiredString(value.phone_number, `buttons[${offset}].phone_number`) }) });
      throw fail(`buttons[${offset}].type is unsupported`, 'boundary_rejected');
    });
    const quoted = payload.reply_to_message_id ? quoteFor(payload.reply_to_message_id, chatId) : undefined;
    return sendNativeFlow(chatId, payload, nativeButtons, quoted);
  }
  if (operation === 'chat.info') {
    if (chatId.endsWith('@g.us')) {
      const metadata = await retryIdempotent(() => sock.groupMetadata(chatId));
      return { chat_id: chatId, is_group: true, name: metadata.subject || null, owner: metadata.owner || null, description: metadata.desc || null, creation: Number(metadata.creation || 0), subject_owner: metadata.subjectOwner || null, subject_time: Number(metadata.subjectTime || 0), description_owner: metadata.descOwner || null, description_time: Number(metadata.descTime || 0), announce: Boolean(metadata.announce), restrict: Boolean(metadata.restrict), is_community: Boolean(metadata.isCommunity), is_community_announce: Boolean(metadata.isCommunityAnnounce), linked_parent: metadata.linkedParent || null, join_approval_mode: Boolean(metadata.joinApprovalMode), member_add_mode: Boolean(metadata.memberAddMode), ephemeral_duration: Number(metadata.ephemeralDuration || 0), participants: (metadata.participants || []).map(item => ({ id: item.id, admin: item.admin || null })), raw_metadata: metadata };
    }
    return { chat_id: chatId, is_group: false, name: normalizeIdentifier(chatId), participants: [] };
  }
  if (!chatId.endsWith('@g.us') && operation.startsWith('group.')) throw fail(`${operation} requires a group/community chat`, 'boundary_rejected');
  if (operation === 'group.participants.update') {
    const participants = Array.isArray(payload.participants) ? payload.participants.map(normalizeJid) : [];
    if (!participants.length || participants.length > 100) throw fail('participants must contain 1..100 entries', 'boundary_rejected');
    const action = requiredString(payload.action, 'action');
    if (!['add', 'remove', 'promote', 'demote'].includes(action)) throw fail('group participant action is unsupported', 'boundary_rejected');
    const results = await retryIdempotent(() => sock.groupParticipantsUpdate(chatId, participants, action));
    return { chat_id: chatId, action, participants, results };
  }
  if (operation === 'group.subject.set') {
    const subject = requiredString(payload.subject, 'subject');
    await retryIdempotent(() => sock.groupUpdateSubject(chatId, subject));
    return { chat_id: chatId, subject };
  }
  if (operation === 'group.description.set') {
    if (typeof payload.description !== 'string') throw fail('description must be text', 'boundary_rejected');
    await retryIdempotent(() => sock.groupUpdateDescription(chatId, payload.description));
    return { chat_id: chatId, description: payload.description };
  }
  if (operation === 'group.setting.set') {
    const setting = requiredString(payload.setting, 'setting');
    if (!['announcement', 'not_announcement', 'locked', 'unlocked'].includes(setting)) throw fail('group setting is unsupported', 'boundary_rejected');
    await retryIdempotent(() => sock.groupSettingUpdate(chatId, setting));
    return { chat_id: chatId, setting };
  }
  if (operation === 'group.invite.get') {
    const code = await retryIdempotent(() => sock.groupInviteCode(chatId));
    return { chat_id: chatId, available: typeof code === 'string' && Boolean(code), invite_code: code || null, invite_url: code ? `https://chat.whatsapp.com/${code}` : null };
  }
  if (operation === 'group.invite.revoke') {
    const code = await boundedCall(() => sock.groupRevokeInvite(chatId));
    return { chat_id: chatId, revoked: true, invite_code: code || null, invite_url: code ? `https://chat.whatsapp.com/${code}` : null };
  }
  if (operation === 'group.join_requests.list') {
    const requests = await retryIdempotent(() => sock.groupRequestParticipantsList(chatId));
    return { chat_id: chatId, requests };
  }
  if (operation === 'group.join_requests.update') {
    const participants = Array.isArray(payload.participants) ? payload.participants.map(normalizeJid) : [];
    if (!participants.length || participants.length > 100) throw fail('participants must contain 1..100 entries', 'boundary_rejected');
    const action = requiredString(payload.action, 'action');
    if (!['approve', 'reject'].includes(action)) throw fail('join request action is unsupported', 'boundary_rejected');
    const results = await retryIdempotent(() => sock.groupRequestParticipantsUpdate(chatId, participants, action));
    return { chat_id: chatId, action, participants, results };
  }
  if (operation === 'group.ephemeral.set') {
    const expiration = requiredInteger(payload.expiration_seconds, 'expiration_seconds', 0, 7776000);
    if (![0, 86400, 604800, 7776000].includes(expiration)) throw fail('ephemeral expiration is unsupported', 'boundary_rejected');
    await retryIdempotent(() => sock.groupToggleEphemeral(chatId, expiration));
    return { chat_id: chatId, expiration_seconds: expiration };
  }
  if (operation === 'group.leave') {
    await boundedCall(() => sock.groupLeave(chatId));
    return { chat_id: chatId, left: true };
  }
  if (operation === 'community.info') {
    const metadata = await requireCommunityMetadata(chatId);
    return { chat_id: chatId, name: metadata.subject || null, owner: metadata.owner || null, description: metadata.desc || null, creation: Number(metadata.creation || 0), announce: Boolean(metadata.announce), restrict: Boolean(metadata.restrict), is_community: true, is_community_announce: Boolean(metadata.isCommunityAnnounce), linked_parent: metadata.linkedParent || null, join_approval_mode: Boolean(metadata.joinApprovalMode), member_add_mode: Boolean(metadata.memberAddMode), participants: (metadata.participants || []).map(item => ({ id: item.id, admin: item.admin || null })), raw_metadata: metadata };
  }
  if (operation.startsWith('community.')) await requireCommunityMetadata(chatId);
  if (operation === 'community.participants.update') {
    const participants = Array.isArray(payload.participants) ? payload.participants.map(normalizeJid) : [];
    if (!participants.length || participants.length > 100) throw fail('participants must contain 1..100 entries', 'boundary_rejected');
    const action = requiredString(payload.action, 'action');
    if (!['add', 'remove', 'promote', 'demote'].includes(action)) throw fail('community participant action is unsupported', 'boundary_rejected');
    const results = await retryIdempotent(() => sock.communityParticipantsUpdate(chatId, participants, action));
    return { chat_id: chatId, action, participants, results };
  }
  if (operation === 'community.subject.set') {
    const subject = requiredString(payload.subject, 'subject');
    await retryIdempotent(() => sock.communityUpdateSubject(chatId, subject));
    return { chat_id: chatId, subject };
  }
  if (operation === 'community.description.set') {
    if (typeof payload.description !== 'string') throw fail('description must be text', 'boundary_rejected');
    await retryIdempotent(() => sock.communityUpdateDescription(chatId, payload.description));
    return { chat_id: chatId, description: payload.description };
  }
  if (operation === 'community.setting.set') {
    const setting = requiredString(payload.setting, 'setting');
    if (!['announcement', 'not_announcement', 'locked', 'unlocked'].includes(setting)) throw fail('community setting is unsupported', 'boundary_rejected');
    await retryIdempotent(() => sock.communitySettingUpdate(chatId, setting));
    return { chat_id: chatId, setting };
  }
  if (operation === 'community.invite.get') {
    const code = await retryIdempotent(() => sock.communityInviteCode(chatId));
    return { chat_id: chatId, available: typeof code === 'string' && Boolean(code), invite_code: code || null, invite_url: code ? `https://chat.whatsapp.com/${code}` : null };
  }
  if (operation === 'community.invite.revoke') {
    const code = await boundedCall(() => sock.communityRevokeInvite(chatId));
    return { chat_id: chatId, revoked: true, invite_code: code || null, invite_url: code ? `https://chat.whatsapp.com/${code}` : null };
  }
  if (operation === 'community.linked_groups.list') {
    const result = await retryIdempotent(() => sock.communityFetchLinkedGroups(chatId));
    return { chat_id: chatId, ...result };
  }
  if (operation === 'community.join_requests.list') {
    const requests = await retryIdempotent(() => sock.communityRequestParticipantsList(chatId));
    return { chat_id: chatId, requests };
  }
  if (operation === 'community.join_requests.update') {
    const participants = Array.isArray(payload.participants) ? payload.participants.map(normalizeJid) : [];
    if (!participants.length || participants.length > 100) throw fail('participants must contain 1..100 entries', 'boundary_rejected');
    const action = requiredString(payload.action, 'action');
    if (!['approve', 'reject'].includes(action)) throw fail('community join request action is unsupported', 'boundary_rejected');
    const results = await retryIdempotent(() => sock.communityRequestParticipantsUpdate(chatId, participants, action));
    return { chat_id: chatId, action, participants, results };
  }
  if (operation === 'community.member_add_mode.set') {
    const mode = requiredString(payload.mode, 'mode');
    if (!['admin_add', 'all_member_add'].includes(mode)) throw fail('community member add mode is unsupported', 'boundary_rejected');
    await retryIdempotent(() => sock.communityMemberAddMode(chatId, mode));
    return { chat_id: chatId, mode };
  }
  if (operation === 'community.join_approval_mode.set') {
    const mode = requiredString(payload.mode, 'mode');
    if (!['on', 'off'].includes(mode)) throw fail('community join approval mode is unsupported', 'boundary_rejected');
    await retryIdempotent(() => sock.communityJoinApprovalMode(chatId, mode));
    return { chat_id: chatId, mode };
  }
  if (operation === 'community.ephemeral.set') {
    const expiration = requiredInteger(payload.expiration_seconds, 'expiration_seconds', 0, 7776000);
    if (![0, 86400, 604800, 7776000].includes(expiration)) throw fail('ephemeral expiration is unsupported', 'boundary_rejected');
    await retryIdempotent(() => sock.communityToggleEphemeral(chatId, expiration));
    return { chat_id: chatId, expiration_seconds: expiration };
  }
  if (operation === 'community.leave') {
    await boundedCall(() => sock.communityLeave(chatId));
    return { chat_id: chatId, left: true };
  }
  throw fail(`unsupported operation ${operation}`, 'boundary_rejected');
}

const IDEMPOTENT_EFFECTS = new Set([
  'message.delete', 'reaction.set', 'reaction.clear', 'receipt.read', 'typing.start', 'typing.stop', 'presence.set', 'chat.info',
  'group.participants.update', 'group.subject.set', 'group.description.set', 'group.setting.set', 'group.invite.get',
  'group.join_requests.list', 'group.join_requests.update', 'group.ephemeral.set',
  'community.info', 'community.participants.update', 'community.subject.set', 'community.description.set', 'community.setting.set',
  'community.invite.get', 'community.linked_groups.list', 'community.join_requests.list', 'community.join_requests.update',
  'community.member_add_mode.set', 'community.join_approval_mode.set', 'community.ephemeral.set',
]);

async function executeEffect(operation, payload) {
  const idempotencyKey = requiredString(payload.idempotency_key, 'idempotency_key');
  const requestHash = sha256Bytes(Buffer.from(JSON.stringify({ operation, payload }), 'utf8'));
  const existing = loadEffect(idempotencyKey);
  if (existing) {
    if (existing.request_hash !== requestHash || existing.operation !== operation) throw fail('idempotency key was reused for different bytes', 'idempotency_mismatch');
    if (existing.status === 'succeeded') return existing.result;
    if (existing.status === 'started' || existing.status === 'indeterminate') throw fail('external effect has indeterminate prior state', 'indeterminate_external_effect', { idempotency_key: idempotencyKey });
    if (existing.status === 'failed' && !IDEMPOTENT_EFFECTS.has(operation)) throw fail('non-idempotent effect failed previously and will not be replayed', 'effect_replay_refused');
  }
  ensureEffectCapacity(idempotencyKey);
  const state = { schema_version: 1, idempotency_key: idempotencyKey, request_hash: requestHash, operation, status: 'started', started_at_unix_ms: Date.now() };
  saveEffect(idempotencyKey, state);
  try {
    const result = await executeOperation(operation, payload);
    saveEffect(idempotencyKey, { ...state, status: 'succeeded', completed_at_unix_ms: Date.now(), result });
    return result;
  } catch (error) {
    const status = IDEMPOTENT_EFFECTS.has(operation) ? 'failed' : 'indeterminate';
    saveEffect(idempotencyKey, { ...state, status, completed_at_unix_ms: Date.now(), error: { code: error.code || 'operation_failed', message: sanitizeExternalError(error.message, PROXY_URL), details: sanitizeExternalError(error.details || {}, PROXY_URL) } });
    throw error;
  }
}

async function handleCommand(frame) {
  const id = requiredInteger(frame.id, 'command id', 1);
  const operation = requiredString(frame.operation, 'operation');
  const payload = requiredObject(frame.payload || {}, 'payload');
  try {
    let result;
    if (operation === 'bridge.health') result = { status: connectionState, user_ids: botIds(), pending_events: readdirSync(EVENTS_DIR).filter(name => name.endsWith('.json')).length };
    else if (operation === 'event.ack') result = acknowledgeEvent(requiredInteger(payload.seq, 'event ack seq', 1));
    else if (operation === 'effect.ack') result = acknowledgeEffect(requiredString(payload.idempotency_key, 'effect ack idempotency_key'));
    else if (operation === 'bridge.shutdown') {
      closing = true;
      if (reconnectTimer) clearTimeout(reconnectTimer);
      if (sock && connectionState === 'connected' && ON_DISCONNECT_PRESENCE) {
        try {
          await boundedCall(() => sock.sendPresenceUpdate(ON_DISCONNECT_PRESENCE), Math.min(SEND_TIMEOUT_MS, 5000));
        } catch {}
      }
      sock?.end?.(undefined);
      result = { shutting_down: true };
      setTimeout(() => process.exit(0), 25);
    } else result = await executeEffect(operation, payload);
    writeFrame({ type: 'response', id, ok: true, result });
  } catch (error) {
    writeFrame({ type: 'response', id, ok: false, error: { code: error.code || 'bridge_error', message: sanitizeExternalError(String(error.message || error), PROXY_URL), details: sanitizeExternalError(error.details || {}, PROXY_URL) } });
  }
}

process.on('uncaughtException', error => {
  closing = true;
  statusFrame('fatal', { code: error.code || 'uncaught_exception', message: sanitizeExternalError(error.message, PROXY_URL) });
  setImmediate(() => process.exit(1));
});
process.on('unhandledRejection', error => {
  closing = true;
  statusFrame('fatal', { code: error?.code || 'unhandled_rejection', message: sanitizeExternalError(String(error?.message || error), PROXY_URL) });
  setImmediate(() => process.exit(1));
});
process.on('SIGTERM', () => {
  closing = true;
  const goodbye = sock && connectionState === 'connected' && ON_DISCONNECT_PRESENCE ? sock.sendPresenceUpdate(ON_DISCONNECT_PRESENCE).catch(() => {}) : Promise.resolve();
  goodbye.finally(() => {
    sock?.end?.(undefined);
    process.exit(0);
  });
});
process.on('SIGINT', () => {
  closing = true;
  const goodbye = sock && connectionState === 'connected' && ON_DISCONNECT_PRESENCE ? sock.sendPresenceUpdate(ON_DISCONNECT_PRESENCE).catch(() => {}) : Promise.resolve();
  goodbye.finally(() => {
    sock?.end?.(undefined);
    process.exit(0);
  });
});

const input = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
let commandChain = Promise.resolve();
input.on('line', line => {
  try {
    const frame = JSON.parse(line);
    const command = requiredObject(frame, 'command frame');
    commandChain = commandChain.then(() => handleCommand(command)).catch(error => {
      writeFrame({ type: 'protocol_error', error: { code: error.code || 'command_dispatch_failed', message: sanitizeExternalError(String(error.message || error), PROXY_URL), details: sanitizeExternalError(error.details || {}, PROXY_URL) } });
    });
  } catch (error) {
    writeFrame({ type: 'protocol_error', error: { code: error.code || 'invalid_command_frame', message: sanitizeExternalError(error.message, PROXY_URL), details: sanitizeExternalError(error.details || {}, PROXY_URL) } });
  }
});
input.on('close', () => {
  closing = true;
  sock?.end?.(undefined);
  process.exit(0);
});

writeFrame({ type: 'ready', protocol_version: 1, session_dir: SESSION_DIR, mode: MODE });
replayPendingEvents();
startSocket().catch(error => scheduleReconnect(error.message));
