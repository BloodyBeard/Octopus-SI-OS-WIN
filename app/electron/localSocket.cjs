"use strict";

// Local sockets on Unix and Windows. Node reaches local sockets on Windows
// only as named pipes, so there a socket path names the pipe that the Rust
// `octopus-local-socket` crate serves for it (see `pipe_name` there).
const crypto = require("node:crypto");

function pipeName(socketPath) {
  let text = String(socketPath).replace(/\//g, "\\");
  if (text.startsWith("\\\\?\\UNC\\")) text = `\\\\${text.slice(8)}`;
  else if (text.startsWith("\\\\?\\")) text = text.slice(4);
  const digest = crypto.createHash("sha256").update(text.toLowerCase(), "utf8").digest("hex").slice(0, 32);
  return `\\\\.\\pipe\\octopus-${digest}`;
}

function localSocketAddress(socketPath, platform = process.platform) {
  return platform === "win32" ? pipeName(socketPath) : socketPath;
}

module.exports = { pipeName, localSocketAddress };
