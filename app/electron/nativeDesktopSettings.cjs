"use strict";

const { spawn } = require("node:child_process");
const { createHash, randomBytes } = require("node:crypto");
const { validatePermissionSnapshot } = require("./desktopSurface.cjs");

function createNativeDesktopSettingsHost(options) {
  if (typeof options?.prepareHelper !== "function" || typeof options.homePath !== "function") throw new Error("Computer Use setup requires its installed helper and Home");
  const owners = new Map();
  const preparing = new Map();
  const applicationJobs = new Map();
  let closed = false;

  async function source(owner) {
    owner.current();
    if (closed || !Number.isSafeInteger(owner.id) || owner.id <= 0) throw new Error("Computer Use Settings owner is unavailable");
    const previous = owners.get(owner.id);
    if (previous) { await previous.current(); return previous; }
    const preparation = preparing.get(owner.id);
    if (preparation) { const result = await preparation.promise; owner.current(); await result.current(); return result; }
    const entry = { cancelled: false, promise: null };
    preparing.set(owner.id, entry);
    entry.promise = prepare(owner, entry).finally(() => { if (preparing.get(owner.id) === entry) preparing.delete(owner.id); });
    return entry.promise;
  }

  async function prepare(owner, entry) {
    const check = () => { owner.current(); if (closed || entry.cancelled || preparing.get(owner.id) !== entry) throw new Error("Computer Use Settings preparation was closed"); };
    const prepared = await options.prepareHelper(); check();
    if (prepared.home !== options.homePath() || typeof prepared.current !== "function") throw new Error("Computer Use helper belongs to another Home");
    if (!Number.isSafeInteger(prepared.timeout) || prepared.timeout <= 0 || prepared.timeout > 600000) throw new Error("Computer Use helper has no bounded response deadline");
    await prepared.current(); check();
    const child = (options.spawn || spawn)(prepared.program, ["--permission-setup"], {
      cwd: prepared.directory, env: { PATH: "/usr/bin:/bin:/usr/sbin:/sbin", LANG: "en_US.UTF-8" }, stdio: ["pipe", "pipe", "pipe"],
    });
    const generation = randomBytes(16).toString("hex"), pending = new Map();
    let sequence = 0, buffer = "", exited = false, failure = null;
    const record = {
      home: prepared.home, timeout: prepared.timeout, current: async () => {
        owner.current();
        if (closed || owners.get(owner.id) !== record || exited || record.home !== options.homePath()) throw failure || new Error("Computer Use setup helper is no longer current");
        await prepared.current(); owner.current();
        if (owners.get(owner.id) !== record || exited) throw failure || new Error("Computer Use setup helper changed");
      },
      preference: async enabled => {
        await record.current();
        if (typeof prepared.preference !== "function") throw new Error("Computer Use has no declared preference store");
        const value = await prepared.preference(enabled); await record.current();
        if (!value || value.schema_version !== 1 || typeof value.enabled !== "boolean" || Object.keys(value).length !== 2) throw new Error("Computer Use preference is malformed");
        return value;
      },
      exchange: async (operation, permission, signal) => {
        await record.current();
        if (!["status", "open_settings"].includes(operation) || (operation === "status" ? permission !== null : typeof permission !== "string")) throw new Error("Computer Use setup supports status and explicit System Settings only");
        if (signal?.aborted) throw signal.reason;
        const id = ++sequence;
        const value = await new Promise((resolve, reject) => {
          const cancel = () => fail(signal.reason || new Error("Computer Use Settings closed"));
          const finish = (error, result) => {
            if (!pending.delete(id)) return;
            clearTimeout(timer); signal?.removeEventListener("abort", cancel);
            error ? reject(error) : resolve(result);
          };
          const timer = setTimeout(() => fail(new Error("Computer Use permission outcome is unknown; check status before trying again")), prepared.timeout);
          pending.set(id, { finish }); signal?.addEventListener("abort", cancel, { once: true });
          if (signal?.aborted) { cancel(); return; }
          child.stdin.write(JSON.stringify({ id, action: { type: "permissions", operation, permission } }) + "\n", error => { if (error) fail(error); });
        });
        await record.current(); if (signal?.aborted) throw signal.reason;
        if (!value || typeof value !== "object" || Array.isArray(value) || Object.keys(value).length !== 2
          || !Object.hasOwn(value, "helper") || !Object.hasOwn(value, "permissions")
          || !Number.isSafeInteger(child.pid) || value.helper?.process_id !== child.pid || value.helper?.executable_path !== prepared.program) {
          const error = new Error("Computer Use permission status came from another installed helper"); fail(error); throw error;
        }
        try { return validatePermissionSnapshot({ ...value, schema_version: 1, host_generation: generation }, generation); }
        catch (error) { fail(error); throw error; }
      },
      close: () => {
        if (owners.get(owner.id) === record) owners.delete(owner.id);
        fail(new Error("Computer Use Settings owner closed"));
      },
    };
    const fail = error => {
      if (exited) return;
      failure = error; exited = true;
      if (owners.get(owner.id) === record) owners.delete(owner.id);
      for (const item of [...pending.values()]) item.finish(error);
      child.stdin.destroy(); child.stdout.destroy(); child.stderr.destroy(); child.kill("SIGTERM");
    };
    child.on("error", fail);
    child.on("exit", () => fail(new Error("Computer Use setup helper exited")));
    child.stderr.on("data", () => {});
    child.stdout.setEncoding("utf8");
    child.stdout.on("data", chunk => {
      buffer += chunk;
      if (Buffer.byteLength(buffer) > 128 * 1024) { fail(new Error("Computer Use setup response exceeded its bound")); return; }
      while (buffer.includes("\n")) {
        const at = buffer.indexOf("\n"), line = buffer.slice(0, at); buffer = buffer.slice(at + 1);
        let value;
        try { value = JSON.parse(line); } catch { fail(new Error("Computer Use setup response is malformed")); return; }
        const item = value && typeof value === "object" && !Array.isArray(value) ? pending.get(value.id) : null;
        if (!item || typeof value.ok !== "boolean" || Object.keys(value).some(key => !["id", "ok", "result", "code", "message"].includes(key))) { fail(new Error("Computer Use setup response lost its request identity")); return; }
        item.finish(value.ok ? null : new Error(value.message || "Computer Use setup was refused"), value.result);
      }
    });
    owners.set(owner.id, record);
    try { await record.current(); return record; } catch (error) { record.close(); throw error; }
  }
  async function applicationPermissions(raw, owner) {
    const fields = raw?.operation === "list" ? ["operation"] : ["operation", "sourceId", "bundleId"];
    if (!raw || typeof raw !== "object" || Array.isArray(raw) || !["list", "remove"].includes(raw.operation)
      || Object.keys(raw).length !== fields.length || fields.some(key => !Object.hasOwn(raw,key))
      || !Number.isSafeInteger(owner.id) || owner.id <= 0 || typeof owner.identity !== "string" || !owner.identity) throw new Error("Invalid application Settings request");
    if (raw.operation === "remove" && (typeof raw.bundleId !== "string" || !raw.bundleId || raw.bundleId.trim() !== raw.bundleId
      || Buffer.byteLength(raw.bundleId) > 4096 || raw.bundleId.includes("\0") || !/^[a-f0-9]{64}$/.test(raw.sourceId || ""))) throw new Error("Application Settings removal needs its exact source and bundle");
    const jobs = applicationJobs.get(owner.id) || new Set(), job = { cancelled: false, cancel: null };
    jobs.add(job); applicationJobs.set(owner.id,jobs);
    const check = () => { owner.current(); if (closed || job.cancelled || applicationJobs.get(owner.id) !== jobs || !jobs.has(job)) throw new Error("Application Settings owner closed"); };
    try {
      check(); const prepared = await options.prepareHelper(); check();
      if (prepared.home !== options.homePath() || typeof prepared.current !== "function" || typeof prepared.applicationIdentity !== "string"
        || typeof prepared.preferenceRoot !== "string" || !prepared.preferenceRoot.startsWith("/") || typeof prepared.settingsProgram !== "string" || !prepared.settingsProgram.startsWith("/")
        || !Number.isSafeInteger(prepared.timeout) || prepared.timeout <= 0 || prepared.timeout > 600000) throw new Error("Application Settings has no verified installed owner");
      await prepared.current(); check();
      const sourceId = createHash("sha256").update(JSON.stringify([owner.id,owner.identity,prepared.home,prepared.preferenceRoot,prepared.applicationIdentity])).digest("hex");
      if (raw.operation === "remove" && raw.sourceId !== sourceId) throw new Error("Saved application permission source changed; refresh before removing");
      const value = await new Promise((resolve,reject) => {
        const child = (options.spawn || spawn)(prepared.settingsProgram,["--application-settings",prepared.preferenceRoot],{
          cwd:prepared.directory,env:{PATH:"/usr/bin:/bin:/usr/sbin:/sbin",LANG:"en_US.UTF-8"},stdio:["pipe","pipe","pipe"],
        });
        let settled=false,output=[],length=0,stderrBytes=0;
        const finish = (error,result) => {
          if (settled) return; settled=true;clearTimeout(timer);job.cancel=null;
          if (error) {child.stdin.destroy();child.stdout.destroy();child.stderr.destroy();child.kill("SIGTERM");reject(error);} else resolve(result);
        };
        const timer=setTimeout(()=>finish(new Error("Application Settings outcome is unknown; refresh before another removal")),prepared.timeout);
        job.cancel=()=>finish(new Error("Application Settings owner closed; a dispatched removal may already have completed"));
        child.on("error",error=>finish(error));
        child.stdout.on("data",bytes=>{length+=bytes.length;if(length>16*1024*1024)finish(new Error("Application Settings response exceeded its bound"));else output.push(bytes);});
        child.stderr.on("data",bytes=>{stderrBytes+=bytes.length;if(stderrBytes>262144)finish(new Error("Application Settings diagnostic exceeded its bound"));});
        child.on("close",code=>{
          if(settled)return;
          if(code!==0){finish(new Error("Application Settings was refused by its installed typed reader"));return;}
          try {finish(null,JSON.parse(Buffer.concat(output).toString("utf8")));} catch(error){finish(error);}
        });
        child.stdin.on("error",error=>finish(error));
        child.stdin.end(JSON.stringify({operation:raw.operation,...(raw.operation==="remove"?{bundle_id:raw.bundleId}:{})})+"\n");
      });
      check(); await prepared.current(); check();
      if (prepared.home !== options.homePath() || !value || typeof value !== "object" || Array.isArray(value)
        || Object.keys(value).length !== 3 || value.schema_version !== 1 || typeof value.removed !== "boolean" || !Array.isArray(value.applications)
        || value.applications.length > 1024 || (raw.operation === "list" && value.removed)) throw new Error("Application Settings returned malformed evidence");
      const seen=new Set();
      for(const application of value.applications){
        if(!application || typeof application!=="object" || Array.isArray(application) || Object.keys(application).length!==2
          || !["bundle_id","display_name"].every(key=>typeof application[key]==="string"&&application[key]&&Buffer.byteLength(application[key])<=4096&&!application[key].includes("\0"))
          || seen.has(application.bundle_id))throw new Error("Application Settings returned an invalid native subject");
        seen.add(application.bundle_id);
      }
      if(raw.operation==="remove"&&seen.has(raw.bundleId))throw new Error("Application Settings did not remove its requested subject");
      return {sourceId,applications:value.applications,removed:value.removed};
    } finally { jobs.delete(job); if(!jobs.size&&applicationJobs.get(owner.id)===jobs)applicationJobs.delete(owner.id); }
  }
  function closeOwner(id) { const entry = preparing.get(id); if (entry) { entry.cancelled = true; preparing.delete(id); } owners.get(id)?.close();
    for(const job of applicationJobs.get(id)||[]){job.cancelled=true;job.cancel?.();}applicationJobs.delete(id);
  }
  function stop() { closed = true; for (const id of new Set([...owners.keys(), ...preparing.keys(), ...applicationJobs.keys()])) closeOwner(id); }
  return { source, applicationPermissions, closeOwner, stop };
}

module.exports = { createNativeDesktopSettingsHost };
