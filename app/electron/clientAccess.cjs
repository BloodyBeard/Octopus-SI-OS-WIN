const crypto = require("node:crypto");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const SESSION_TTL_MS = 8 * 60 * 60 * 1000;
const MIN_PASSWORD_LENGTH = 8;
const MIN_LAB_SECRET_LENGTH = 8;
const DEFAULT_LAB_SECRET = "octopus-test";
const MAX_CREDENTIAL_BYTES = 16 * 1024;
const UNLOCK_FAILURES_BEFORE_DELAY = 5;

function createClientAccessController(options = {}) {
  const configuredCredentialPath = String(options.credentialPath || process.env.OCTOPUS_CLIENT_ACCESS_PATH || defaultCredentialPath());
  if (!path.isAbsolute(configuredCredentialPath)) throw new Error("Client credential path must be absolute");
  const credentialPath = path.resolve(configuredCredentialPath);
  const configuredLabCredentialPath = String(options.labCredentialPath || process.env.OCTOPUS_CLIENT_LAB_ACCESS_PATH || path.join(path.dirname(credentialPath), "client-lab-access.json"));
  if (!path.isAbsolute(configuredLabCredentialPath)) throw new Error("Client test credential path must be absolute");
  const labCredentialPath = path.resolve(configuredLabCredentialPath);
  if (credentialPath === labCredentialPath) throw new Error("Administrator and Test credential paths must be different");
  const environmentAdministratorSecret = String(options.administratorSecret ?? process.env.OCTOPUS_CLIENT_ADMIN_SECRET ?? "");
  const environmentLabSecret = String(options.labSecret ?? process.env.OCTOPUS_CLIENT_LAB_SECRET ?? "");
  const defaultLabSecret = String(options.defaultLabSecret ?? DEFAULT_LAB_SECRET);
  const now = typeof options.now === "function" ? options.now : Date.now;
  const openAccess = String(options.openAccess ?? process.env.OCTOPUS_CLIENT_ACCESS_OPEN ?? "") === "1";
  const sessions = new Map();
  let failedUnlocks = 0;
  let unlockBlockedUntil = 0;
  if (environmentAdministratorSecret && environmentAdministratorSecret.length < MIN_PASSWORD_LENGTH) {
    throw new Error(`OCTOPUS_CLIENT_ADMIN_SECRET must contain at least ${MIN_PASSWORD_LENGTH} characters`);
  }
  if (environmentLabSecret && environmentLabSecret.length < MIN_LAB_SECRET_LENGTH) {
    throw new Error(`OCTOPUS_CLIENT_LAB_SECRET must contain at least ${MIN_LAB_SECRET_LENGTH} characters`);
  }
  if (defaultLabSecret.length < MIN_LAB_SECRET_LENGTH) throw new Error("Default client test password is invalid");
  if (environmentAdministratorSecret && verifyLabSecret(environmentAdministratorSecret)) {
    throw new Error("Administrator and Test passwords must be different.");
  }

  function status(token) {
    const session = readSession(token);
    const configured = Boolean(environmentAdministratorSecret || fs.existsSync(credentialPath));
    return {
      configured,
      labAvailable: configured,
      authenticated: Boolean(session),
      profile: session?.profile ?? null,
      expiresAt: session ? new Date(session.expiresAt).toISOString() : null,
    };
  }

  function configure(password) {
    if (environmentAdministratorSecret) throw new Error("Administrator password is configured by OCTOPUS_CLIENT_ADMIN_SECRET.");
    const value = requiredPassword(password);
    if (fs.existsSync(credentialPath)) throw new Error("A password is already configured on this device.");
    if (verifyLabSecret(value)) throw new Error("Administrator and Test passwords must be different.");
    const salt = crypto.randomBytes(16);
    const hash = derive(value, salt);
    writeCredential({
      version: 1,
      kdf: "scrypt",
      salt: salt.toString("base64"),
      hash: hash.toString("base64"),
      createdAt: new Date(now()).toISOString(),
    });
    return createSession("live");
  }

  function unlock(candidate) {
    const blockedFor = unlockBlockedUntil - now();
    if (blockedFor > 0) {
      throw new Error(`Too many unsuccessful unlock attempts. Try again in ${Math.max(1, Math.ceil(blockedFor / 1000))} seconds.`);
    }
    const value = String(candidate ?? "");
    const record = environmentAdministratorSecret ? null : readCredential();
    if (!environmentAdministratorSecret && !record) throw new Error("Configure the Administrator password before unlocking Octopus.");
    if (verifyAdministratorSecret(value, record)) {
      resetUnlockFailures();
      return createSession("live");
    }
    if (verifyLabSecret(value)) {
      resetUnlockFailures();
      return createSession("lab");
    }
    failedUnlocks += 1;
    if (failedUnlocks >= UNLOCK_FAILURES_BEFORE_DELAY) {
      const exponent = Math.min(8, failedUnlocks - UNLOCK_FAILURES_BEFORE_DELAY);
      unlockBlockedUntil = now() + Math.min(5 * 60 * 1000, 2_000 * (2 ** exponent));
    }
    throw new Error("Incorrect password.");
  }

  function resetUnlockFailures() {
    failedUnlocks = 0;
    unlockBlockedUntil = 0;
  }

  function requireSession(token, allowedProfiles = ["live", "lab"]) {
    const session = readSession(token);
    if (!session) throw new Error("Client access session is locked or expired");
    if (!allowedProfiles.includes(session.profile)) {
      throw new Error(`Client access profile ${session.profile} cannot use this capability`);
    }
    return session;
  }

  function lock(token) {
    if (typeof token === "string" && token) sessions.delete(token);
    return status("");
  }

  function labAccess(token) {
    requireSession(token, ["live"]);
    return {
      source: environmentLabSecret ? "environment" : fs.existsSync(labCredentialPath) ? "administrator" : "default",
      minimumLength: MIN_LAB_SECRET_LENGTH,
    };
  }

  function accessDetails(token) {
    const session = requireSession(token);
    const record = environmentAdministratorSecret ? null : readCredential();
    const live = session.profile === "live";
    return {
      profile: session.profile,
      minimumLength: MIN_PASSWORD_LENGTH,
      administrator: {
        configured: Boolean(environmentAdministratorSecret || record),
        source: environmentAdministratorSecret ? "environment" : "credential",
        createdAt: typeof record?.createdAt === "string" ? record.createdAt : null,
        updatedAt: typeof record?.updatedAt === "string" ? record.updatedAt : null,
      },
      test: {
        available: Boolean(environmentAdministratorSecret || record),
        source: live
          ? environmentLabSecret ? "environment" : fs.existsSync(labCredentialPath) ? "administrator" : "default"
          : null,
      },
    };
  }

  function updateAdministratorAccess(token, currentPassword, password) {
    requireSession(token, ["live"]);
    if (environmentAdministratorSecret) throw new Error("Administrator password is managed by OCTOPUS_CLIENT_ADMIN_SECRET.");
    const record = readCredential();
    if (!record || !verify(String(currentPassword ?? ""), record)) {
      throw new Error("Current Administrator password is incorrect.");
    }
    const value = requiredPassword(password);
    if (verifyLabSecret(value)) throw new Error("Administrator and Test passwords must be different.");
    const salt = crypto.randomBytes(16);
    writeCredential({
      version: 1,
      kdf: "scrypt",
      salt: salt.toString("base64"),
      hash: derive(value, salt).toString("base64"),
      createdAt: typeof record.createdAt === "string" ? record.createdAt : new Date(now()).toISOString(),
      updatedAt: new Date(now()).toISOString(),
    });
    for (const [sessionToken, session] of sessions) {
      if (session.profile === "live" && sessionToken !== token) sessions.delete(sessionToken);
    }
    return accessDetails(token);
  }

  function updateLabAccess(token, password) {
    requireSession(token, ["live"]);
    if (environmentLabSecret) throw new Error("Test password is managed by OCTOPUS_CLIENT_LAB_SECRET.");
    const value = String(password ?? "");
    if (value.trim().length < MIN_LAB_SECRET_LENGTH) {
      throw new Error(`Use at least ${MIN_LAB_SECRET_LENGTH} characters for the Test password.`);
    }
    const administrator = readCredential();
    if (administrator && verify(value, administrator)) {
      throw new Error("Administrator and Test passwords must be different.");
    }
    const salt = crypto.randomBytes(16);
    writeLabCredential({
      version: 1,
      kdf: "scrypt",
      salt: salt.toString("base64"),
      hash: derive(value, salt).toString("base64"),
      updatedAt: new Date(now()).toISOString(),
    });
    for (const [sessionToken, session] of sessions) {
      if (session.profile === "lab") sessions.delete(sessionToken);
    }
    return labAccess(token);
  }

  function credentialFingerprint() {
    if (environmentAdministratorSecret) {
      return crypto.createHash("sha256").update("environment\0").update(environmentAdministratorSecret).digest("hex");
    }
    const record = readCredential();
    if (!record) return null;
    return crypto.createHash("sha256").update(JSON.stringify(record)).digest("hex");
  }

  function unlockWithTouchId() {
    if (!environmentAdministratorSecret && !readCredential()) throw new Error("Configure the Administrator password before unlocking Octopus.");
    resetUnlockFailures();
    return createSession("live");
  }

  function createSession(profile) {
    const token = crypto.randomBytes(32).toString("base64url");
    const expiresAt = now() + SESSION_TTL_MS;
    sessions.set(token, { profile, expiresAt });
    return { token, profile, expiresAt: new Date(expiresAt).toISOString() };
  }

  function readSession(token) {
    if (openAccess) return { profile: "live", expiresAt: now() + SESSION_TTL_MS };
    if (typeof token !== "string" || !token) return null;
    const session = sessions.get(token);
    if (!session) return null;
    if (session.expiresAt <= now()) {
      sessions.delete(token);
      return null;
    }
    return session;
  }

  function readCredential() {
    return readCredentialFile(credentialPath, "Client credential");
  }

  function writeCredential(record) {
    const directory = path.dirname(credentialPath);
    fs.mkdirSync(directory, { recursive: true, mode: 0o700 });
    const temporary = `${credentialPath}.${process.pid}.${crypto.randomBytes(6).toString("hex")}.tmp`;
    fs.writeFileSync(temporary, `${JSON.stringify(record, null, 2)}\n`, { mode: 0o600, flag: "wx" });
    fs.renameSync(temporary, credentialPath);
    fs.chmodSync(credentialPath, 0o600);
  }

  function readLabCredential() {
    return readCredentialFile(labCredentialPath, "Client test credential");
  }

  function writeLabCredential(record) {
    const directory = path.dirname(labCredentialPath);
    fs.mkdirSync(directory, { recursive: true, mode: 0o700 });
    const temporary = `${labCredentialPath}.${process.pid}.${crypto.randomBytes(6).toString("hex")}.tmp`;
    fs.writeFileSync(temporary, `${JSON.stringify(record, null, 2)}\n`, { mode: 0o600, flag: "wx" });
    fs.renameSync(temporary, labCredentialPath);
    fs.chmodSync(labCredentialPath, 0o600);
  }

  function verifyLabSecret(value) {
    if (environmentLabSecret) return secretEqual(value, environmentLabSecret);
    const credential = readLabCredential();
    if (credential) return verify(value, credential);
    return secretEqual(value, defaultLabSecret);
  }

  function verifyAdministratorSecret(value, credential) {
    if (environmentAdministratorSecret) return secretEqual(value, environmentAdministratorSecret);
    return credential ? verify(value, credential) : false;
  }

  return {
    accessDetails,
    configure,
    credentialFingerprint,
    credentialPath,
    labAccess,
    labCredentialPath,
    lock,
    requireSession,
    status,
    unlock,
    unlockWithTouchId,
    updateAdministratorAccess,
    updateLabAccess,
  };
}

function defaultCredentialPath() {
  if (process.platform === "darwin") return path.join(os.homedir(), "Library", "Application Support", "Octopus", "client-access.json");
  if (process.platform === "win32") return path.join(process.env.APPDATA || path.join(os.homedir(), "AppData", "Roaming"), "Octopus", "client-access.json");
  return path.join(process.env.XDG_CONFIG_HOME || path.join(os.homedir(), ".config"), "Octopus", "client-access.json");
}

function requiredPassword(value) {
  const text = String(value ?? "");
  if (text.trim().length < MIN_PASSWORD_LENGTH) throw new Error(`Use at least ${MIN_PASSWORD_LENGTH} characters.`);
  return text;
}

function derive(value, salt) {
  return crypto.scryptSync(value, salt, 32, { N: 32768, r: 8, p: 1, maxmem: 64 * 1024 * 1024 });
}

function verify(value, record) {
  try {
    const salt = Buffer.from(record.salt, "base64");
    const expected = Buffer.from(record.hash, "base64");
    const actual = derive(value, salt);
    return expected.length === actual.length && crypto.timingSafeEqual(expected, actual);
  } catch {
    throw new Error("Client credential file is invalid");
  }
}

function readCredentialFile(file, label) {
  let stat;
  try {
    stat = fs.lstatSync(file);
  } catch (error) {
    if (error && error.code === "ENOENT") return null;
    throw new Error(`${label} cannot be inspected: ${formatError(error)}`);
  }
  if (!stat.isFile() || stat.isSymbolicLink()) throw new Error(`${label} path is not a regular file`);
  if (stat.size <= 0 || stat.size > MAX_CREDENTIAL_BYTES) throw new Error(`${label} file has an invalid size`);
  if (process.platform !== "win32" && (stat.mode & 0o077) !== 0) {
    throw new Error(`${label} file permissions are too broad; expected mode 0600`);
  }
  let value;
  try {
    value = JSON.parse(fs.readFileSync(file, "utf8"));
  } catch (error) {
    throw new Error(`${label} cannot be read: ${formatError(error)}`);
  }
  if (value?.version !== 1 || value?.kdf !== "scrypt" || typeof value.salt !== "string" || typeof value.hash !== "string") {
    throw new Error(`${label} file is invalid`);
  }
  const salt = decodeCanonicalBase64(value.salt);
  const hash = decodeCanonicalBase64(value.hash);
  if (!salt || salt.length !== 16 || !hash || hash.length !== 32) throw new Error(`${label} file is invalid`);
  return value;
}

function decodeCanonicalBase64(value) {
  try {
    const decoded = Buffer.from(value, "base64");
    return decoded.toString("base64") === value ? decoded : null;
  } catch {
    return null;
  }
}

function secretEqual(left, right) {
  const leftHash = crypto.createHash("sha256").update(left).digest();
  const rightHash = crypto.createHash("sha256").update(right).digest();
  return crypto.timingSafeEqual(leftHash, rightHash);
}

function formatError(error) {
  return error instanceof Error ? error.message : String(error);
}

module.exports = { createClientAccessController, defaultCredentialPath };
