"use strict";

const fs = require("node:fs");
const path = require("node:path");
const { randomUUID } = require("node:crypto");

function absolute(value, name) {
  if (typeof value !== "string" || !value.trim() || value.length > 4096 || value.includes("\0") || !path.isAbsolute(value)) {
    throw new Error(`${name} must be an absolute path`);
  }
  return path.resolve(value);
}

function readSelection(file) {
  let fd;
  try { fd = fs.openSync(file, fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW || 0)); }
  catch (error) { if (error.code === "ENOENT") return null; throw error; }
  try {
    const stat = fs.fstatSync(fd);
    if (!stat.isFile() || stat.size > 16384 || stat.nlink !== 1
        || process.platform !== "win32" && (stat.uid !== process.getuid() || (stat.mode & 0o077) !== 0)) {
      throw new Error("Client launch selection must be an owned private file");
    }
    const value = JSON.parse(fs.readFileSync(fd, "utf8"));
    if (!value || value.schema_version !== 1 || Object.keys(value).length !== 3
        || value.home !== absolute(value.home, "Saved Home") || value.codex_home !== absolute(value.codex_home, "Saved Codex Home")) {
      throw new Error("Client launch selection is malformed");
    }
    return { home: value.home, codexHome: value.codex_home };
  } finally { fs.closeSync(fd); }
}

function resolveClientLaunchSelection({ selectionPath, userRoot, env }) {
  const configuredHome = env.OCTOPUS_HOME?.trim();
  const configuredCodex = env.CODEX_HOME?.trim();
  const saved = configuredHome ? null : readSelection(selectionPath);
  return {
    home: absolute(configuredHome || saved?.home || path.join(userRoot, ".octopus"), "OCTOPUS_HOME"),
    codexHome: configuredCodex ? absolute(configuredCodex, "CODEX_HOME") : saved?.codexHome,
  };
}

function saveClientLaunchSelection(file, selection) {
  const value = { schema_version: 1, home: absolute(selection.home, "Home"), codex_home: absolute(selection.codexHome, "Codex Home") };
  const root = path.dirname(file);
  fs.mkdirSync(root, { recursive: true, mode: 0o700 });
  const stat = fs.lstatSync(root);
  if (!stat.isDirectory() || stat.isSymbolicLink() || process.platform !== "win32" && stat.uid !== process.getuid()) {
    throw new Error("Client launch selection directory is not owned by this user");
  }
  const bytes = `${JSON.stringify(value)}\n`;
  try {
    const saved = readSelection(file);
    if (saved?.home === value.home && saved?.codexHome === value.codex_home) return;
  } catch (error) {
    throw new Error(`Client launch selection cannot be updated: ${error.message}`);
  }
  const temporary = path.join(root, `.launch-${randomUUID()}.tmp`);
  const fd = fs.openSync(temporary, "wx", 0o600);
  try { fs.writeFileSync(fd, bytes); fs.fsyncSync(fd); }
  finally { fs.closeSync(fd); }
  try {
    fs.renameSync(temporary, file);
    if (process.platform !== "win32") {
      const directory = fs.openSync(root, fs.constants.O_RDONLY | fs.constants.O_DIRECTORY | fs.constants.O_NOFOLLOW);
      try { fs.fsyncSync(directory); } finally { fs.closeSync(directory); }
    }
  } finally { fs.rmSync(temporary, { force: true }); }
}

module.exports = { resolveClientLaunchSelection, saveClientLaunchSelection };
