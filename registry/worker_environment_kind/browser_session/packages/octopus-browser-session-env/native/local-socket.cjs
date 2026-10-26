'use strict';
// Local sockets and owner-only folders on Unix and Windows.
//
// Windows: Node reaches local sockets only as named pipes, so a socket path is
// served as the pipe `octopus-local-socket` (Rust) derives from it. Node cannot
// read or write Windows access lists, so `restrictToOwner` uses the system
// icacls tool; the Rust browser door checks the result before it trusts the
// folder.
const crypto = require('node:crypto');
const path = require('node:path');
const { execFileSync } = require('node:child_process');

function pipeName(socketPath) {
  let text = String(socketPath).replace(/\//g, '\\');
  if (text.startsWith('\\\\?\\UNC\\')) text = `\\\\${text.slice(8)}`;
  else if (text.startsWith('\\\\?\\')) text = text.slice(4);
  const digest = crypto.createHash('sha256').update(text.toLowerCase(), 'utf8').digest('hex').slice(0, 32);
  return `\\\\.\\pipe\\octopus-${digest}`;
}

function localSocketAddress(socketPath, platform = process.platform) {
  return platform === 'win32' ? pipeName(socketPath) : socketPath;
}

function systemTool(name) {
  const root = process.env.SystemRoot || process.env.SYSTEMROOT;
  if (!root || !path.isAbsolute(root)) throw new Error('SystemRoot is not set');
  return path.join(root, 'System32', name);
}

let currentUserSid;
function userSid() {
  if (currentUserSid) return currentUserSid;
  const output = execFileSync(systemTool('whoami.exe'), ['/user', '/fo', 'csv', '/nh'], { encoding: 'utf8', windowsHide: true });
  const match = /"(S-1-[0-9-]+)"\s*$/.exec(output.trim());
  if (!match) throw new Error('The current user SID is unavailable');
  currentUserSid = match[1];
  return currentUserSid;
}

// The Windows counterpart of chmod 0700 on a folder: a protected access list
// granting only the current user and SYSTEM, inherited by everything inside.
function restrictToOwner(directory) {
  execFileSync(systemTool('icacls.exe'), [directory, '/inheritance:r', '/grant:r', `*${userSid()}:(OI)(CI)F`, '*S-1-5-18:(OI)(CI)F', '/q'], { windowsHide: true, stdio: 'ignore' });
}

module.exports = { pipeName, localSocketAddress, restrictToOwner };
