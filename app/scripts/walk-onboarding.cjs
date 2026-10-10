const crypto = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");
const { Cdp, EXIT, createRedactor, failure, readSelectedAuth, sleep, writeJson } = require("./onboarding-support.cjs");

async function walk(args) {
  if (args.length !== 2 || args[0] !== "--manifest" || !path.isAbsolute(args[1])) throw failure("Usage: walk-onboarding.cjs --manifest /absolute/private/launch.json (created by dev:onboarding).", EXIT.arguments);
  const manifest = JSON.parse(fs.readFileSync(args[1], "utf8"));
  if (manifest.schema_version !== 1 || !/^http:\/\/127\.0\.0\.1:\d+$/.test(manifest.endpoint) || !manifest.browserPath?.startsWith("/devtools/browser/") || manifest.bundleRoot !== path.join(manifest.repoRoot, "app/build/Octopus.app") || !manifest.home.startsWith("/private/tmp/octopus-onb-") || !manifest.reportRoot.startsWith(path.join(manifest.repoRoot, ".private/onboarding") + path.sep)) throw failure("The launch manifest is not an isolated packaged onboarding run.", EXIT.arguments);
  process.kill(manifest.appPid, 0);
  let report = JSON.parse(fs.readFileSync(manifest.reportFile, "utf8"));
  const credentialFile = process.env.OCTOPUS_ONBOARDING_REDACTION_FILE;
  const credentialRedactor = credentialFile ? readSelectedAuth(credentialFile).redact : createRedactor();
  const password = crypto.randomBytes(24).toString("base64url");
  const redact = (value) => credentialRedactor(String(value).split(password).join("[REDACTED]"));
  const deadline = Date.now() + manifest.timeoutMs;
  let cdp;
  let shot = 0;
  let boundaryCode = EXIT.launch;
  const save = () => writeJson(manifest.reportFile, report, redact);
  const mark = (name, evidence = {}) => {
    report.checks.push({ ...evidence, name, at: new Date().toISOString() });
    save();
    console.log(`[${new Date().toISOString()}] verified ${name}`);
  };
  const enterBoundary = (name, code) => { report.boundary = name; boundaryCode = code; save(); };
  const evaluate = (expression) => cdp.evaluate(expression);
  const has = (selector) => evaluate(`Boolean(document.querySelector(${JSON.stringify(selector)}))`);
  const body = () => evaluate("document.body.innerText.slice(0,18000)");
  async function waitFor(name, check, timeoutMs = 90000) {
    const end = Math.min(deadline, Date.now() + timeoutMs);
    while (Date.now() < end) {
      const result = await check();
      if (result) return result;
      await sleep(350);
    }
    throw failure(`Timed out at ${name}.`, boundaryCode);
  }
  async function screenshot(name) {
    const result = await cdp.send("Page.captureScreenshot", { format: "png" });
    const file = path.join(manifest.reportRoot, `${String(++shot).padStart(2, "0")}-${name}.png`);
    fs.writeFileSync(file, Buffer.from(result.data, "base64"), { mode: 0o600 });
    return file;
  }
  async function snapshot(name) {
    const state = await evaluate(`(() => ({title:document.title,url:location.href,text:document.body.innerText.slice(0,18000),headings:[...document.querySelectorAll('h1,h2')].map(e=>e.textContent),buttons:[...document.querySelectorAll('button,[role="button"]')].filter(e=>e.getBoundingClientRect().width&&e.getBoundingClientRect().height&&!e.closest('[inert],[aria-hidden="true"]')).map(e=>({label:e.getAttribute('aria-label')||e.textContent.trim(),disabled:e.disabled||e.getAttribute('aria-disabled')==='true'})),alerts:[...document.querySelectorAll('[role="alert"]')].map(e=>e.innerText)}))()`);
    writeJson(path.join(manifest.reportRoot, `${String(shot + 1).padStart(2, "0")}-${name}.json`), state, redact);
    return { screenshot: await screenshot(name), state };
  }
  async function click(selector, label) {
    const point = await evaluate(`(() => {const list=[...document.querySelectorAll(${JSON.stringify(selector)})];const el=list.find(e=>!e.disabled&&e.getAttribute('aria-disabled')!=='true'&&!e.closest('[inert],[aria-hidden="true"]')&&e.getBoundingClientRect().width&&e.getBoundingClientRect().height${label ? `&&(e.getAttribute('aria-label')===${JSON.stringify(label)}||e.textContent.trim()===${JSON.stringify(label)})` : ""});if(!el)return null;el.scrollIntoView({block:'center'});const r=el.getBoundingClientRect();const x=r.left+r.width/2,y=r.top+r.height/2;const hit=document.elementFromPoint(x,y);return {x,y,rect:{x:r.x,y:r.y,width:r.width,height:r.height},hitMatches:el===hit||el.contains(hit),hit:hit?{tag:hit.tagName,class:hit.className,aria:hit.getAttribute('aria-label')}:null,viewport:{width:innerWidth,height:innerHeight}};})()`);
    if (!point) throw failure(`No enabled visible control: ${label || selector}`, boundaryCode);
    if (!point.hitMatches) {
      writeJson(path.join(manifest.reportRoot, "blocked-control-hit-test.json"), { selector, label, ...point }, redact);
      throw failure(`Control center is obscured: ${label || selector}; inspect blocked-control-hit-test.json.`, boundaryCode);
    }
    if (selector.includes("composer-primary-action")) {
      writeJson(path.join(manifest.reportRoot, `${report.boundary}-send-hit-test.json`), { selector, ...point }, redact);
    }
    const coordinates = { x: point.x, y: point.y };
    await cdp.send("Input.dispatchMouseEvent", { type: "mousePressed", button: "left", clickCount: 1, ...coordinates });
    await cdp.send("Input.dispatchMouseEvent", { type: "mouseReleased", button: "left", clickCount: 1, ...coordinates });
  }
  async function key(keyName, code, virtualKey, text) {
    await cdp.send("Input.dispatchKeyEvent", { type: "keyDown", key: keyName, code, windowsVirtualKeyCode: virtualKey, nativeVirtualKeyCode: virtualKey, ...(text ? { text } : {}) });
    await cdp.send("Input.dispatchKeyEvent", { type: "keyUp", key: keyName, code, windowsVirtualKeyCode: virtualKey, nativeVirtualKeyCode: virtualKey });
  }
  async function type(selector, text) {
    await click(selector);
    await cdp.send("Input.insertText", { text });
  }
  async function readSetupEvidence() {
    const evidence = await evaluate("Promise.all([window.octopusTransport.clientSetupReadCurrent(),window.octopusTransport.workspaceSetupState()]).then(([setup,workspace])=>({setup,workspace}))");
    writeJson(path.join(manifest.reportRoot, "setup-state.json"), evidence, redact);
    return evidence;
  }
  async function captureRuntimeEvidence(name) {
    const evidence = await evaluate("window.octopusTransport.bootStatus().then(async boot=>({boot,terminal:boot.ready?await window.octopusTransport.openLaunchTerminal({}):null}))");
    writeJson(path.join(manifest.reportRoot, `${name}-runtime.json`), evidence, redact);
  }
  async function assertNoDeliveryFailure(scope) {
    const error = await evaluate(`document.querySelector(${JSON.stringify(`${scope} .composer-queue-row[data-message-status="unknown"],${scope} .composer-queue-row[data-message-status="failed"]`)})?.innerText||''`);
    if (error) throw failure(`Native message delivery failed: ${error}`, boundaryCode);
  }
  const projection = (method) => evaluate(`window.octopusTransport.projectionRead({contract_version:1,method:${JSON.stringify(method)}})`);
  async function readAgentCatalog() {
    const response = await projection("projection.operator.chat.contexts");
    return (response.contexts || []).flatMap((context) => (context.workers || [])
      .filter((worker) => worker.client_agent_id && worker.client_template_digest && !worker.client_agent_deleted_at_ms)
      .map((worker) => ({ id: worker.client_agent_id, name: worker.client_agent_name, projectEeId: context.ee_id, workerInterfaceId: worker.id, providerInterfaceId: worker.client_template_provider_interface_id, templateDigest: worker.client_template_digest, catalogDigest: context.agent_catalog_digest })));
  }
  async function createTaskAgent() {
    const initialAgents = await readAgentCatalog();
    writeJson(path.join(manifest.reportRoot, "task-agent-catalog-before.json"), initialAgents, redact);
    await waitFor("create Agent action", () => has('button[aria-label^="Create agent"]:not(:disabled)'));
    await click('button[aria-label^="Create agent"]');
    await waitFor("create Agent dialog", () => has(".agent-preset-dialog #octopus-agent-name"));
    const name = "Onboarding Calculator " + crypto.randomBytes(6).toString("hex");
    await type(".agent-preset-dialog #octopus-agent-name", name);
    const projectControl = await evaluate("[...document.querySelectorAll('.agent-preset-dialog .oc-select-field')].find(e=>e.querySelector('.oc-field__label')?.textContent==='Project')?.querySelector('button')?.id");
    if (!projectControl) throw failure("The Agent dialog has no Project selector.", EXIT.task);
    await click(`.agent-preset-dialog button[id=${JSON.stringify(projectControl)}]`);
    await waitFor("Agent Project choices", () => has('[role="menu"][aria-label="Project"] [role="menuitem"]'));
    const projectName = await evaluate("[...document.querySelectorAll('[role=menu][aria-label=Project] [role=menuitem]')].find(e=>!e.disabled&&e.getAttribute('aria-disabled')!=='true'&&e.getAttribute('aria-label')!=='No project')?.getAttribute('aria-label')");
    if (!projectName) throw failure("The Agent dialog offers no configured Project.", EXIT.task);
    await click('[role="menu"][aria-label="Project"] [role="menuitem"]', projectName);
    await waitFor("Agent Project selected", () => evaluate(`document.querySelector(${JSON.stringify(`.agent-preset-dialog button[id=${JSON.stringify(projectControl)}]`)})?.innerText.trim()===${JSON.stringify(projectName)}`));
    const homes = (await projection("projection.operator.chats.list")).chats.filter((run) => run.client_surface_role === "home");
    if (homes.length !== 1 || !homes[0].provider_interface_id) throw failure("The replied Home has no unique provider binding for the Agent Model choice.", EXIT.task);
    const providerCatalog = await evaluate("window.octopusTransport.providerInterfaces()");
    const modelProvider = providerCatalog.providers.filter((provider) => provider.id === homes[0].provider_interface_id);
    if (modelProvider.length !== 1 || !modelProvider[0].label) throw failure("The replied Home provider has no unique declared Model label.", EXIT.task);
    const modelControl = await evaluate("[...document.querySelectorAll('.agent-preset-dialog .oc-select-field')].find(e=>e.querySelector('.oc-field__label')?.textContent==='Model')?.querySelector('button')?.id");
    if (!modelControl) throw failure("The Agent dialog has no Model selector.", EXIT.task);
    await click(`.agent-preset-dialog button[id=${JSON.stringify(modelControl)}]`);
    await waitFor("Agent Model menu", () => has('[role="menu"][aria-label="Model"]'));
    const modelChoice = await evaluate(`(() => {const e=[...document.querySelectorAll('[role=menu][aria-label=Model] [role=menuitem]')].find(e=>!e.disabled&&e.getAttribute('aria-disabled')!=='true'&&e.getAttribute('aria-label')===${JSON.stringify(modelProvider[0].label)});return e?{label:e.getAttribute('aria-label'),text:e.innerText,providerInterfaceId:${JSON.stringify(modelProvider[0].id)}}:null;})()`);
    if (!modelChoice?.label) throw failure("The Agent dialog offers no enabled Model choice for the replied Home provider.", EXIT.task);
    await snapshot("task-agent-model-choices");
    await click('[role="menu"][aria-label="Model"] [role="menuitem"]', modelChoice.label);
    await waitFor("Agent Model selected", () => evaluate(`document.querySelector(${JSON.stringify(`.agent-preset-dialog button[id=${JSON.stringify(modelControl)}]`)})?.innerText.trim()===${JSON.stringify(modelChoice.label)}`));
    mark("ordinary task Agent model explicitly selected", { modelChoice });
    await waitFor("Agent creation enabled", () => has('.agent-preset-dialog button[type="submit"]:not(:disabled)'));
    await snapshot("create-task-agent");
    await click('.agent-preset-dialog button[type="submit"]');
    const agent = await waitFor("created Agent descriptor", async () => {
      if (await has(".agent-preset-dialog #octopus-agent-name")) return false;
      const matches = (await readAgentCatalog()).filter((candidate) => candidate.name === name);
      if (matches.length > 1) throw failure("The created Agent has conflicting descriptor identities.", EXIT.task);
      return matches[0];
    });
    if (!agent.id || !agent.projectEeId || !agent.workerInterfaceId || !agent.providerInterfaceId || !agent.templateDigest || !agent.catalogDigest) throw failure("The created Agent has incomplete descriptor evidence.", EXIT.task);
    if (agent.providerInterfaceId !== modelChoice.providerInterfaceId) throw failure("The created Agent does not bind to the Model selected through UI.", EXIT.task);
    report.taskAgent = { initialAgentCount: initialAgents.length, createdThroughUi: true, projectName, modelChoice, ...agent };
    writeJson(path.join(manifest.reportRoot, "task-agent-created.json"), report.taskAgent, redact);
    await snapshot("task-agent-created");
    mark("ordinary task Agent created through UI", report.taskAgent);
    return report.taskAgent;
  }
  function ledgerEvidence(name, requireMovements = true) {
    const ledger = path.join(manifest.home, "ledger/movements.jsonl");
    if (!fs.existsSync(ledger)) throw failure("The fresh Home has no artifact movement ledger.", boundaryCode);
    const rows = fs.readFileSync(ledger, "utf8").trim().split("\n").filter(Boolean).map((line) => JSON.parse(line));
    const evidence = { path: ledger, rowCount: rows.length, tail: rows.slice(-40).map((row) => ({ seq: row.seq, kind: row.kind, from: row.from, to: row.to, artifact: row.artifact, trace_id: row.trace_id })) };
    writeJson(path.join(manifest.reportRoot, `${name}-movements.json`), evidence, redact);
    if (requireMovements && !rows.length) throw failure("The fresh Home movement ledger is empty after its reply.", boundaryCode);
    return { path: ledger, rowCount: rows.length };
  }
  try {
    const version = await (await fetch(manifest.endpoint + "/json/version", { signal: AbortSignal.timeout(5000) })).json();
    if (new URL(version.webSocketDebuggerUrl).pathname !== manifest.browserPath) throw failure("Debugger browser identity differs from the owned launch.", EXIT.launch);
    const page = await waitFor("packaged renderer", async () => {
      const list = await (await fetch(manifest.endpoint + "/json/list", { signal: AbortSignal.timeout(5000) })).json();
      return list.find((entry) => entry.type === "page" && entry.url.split("?")[0] === manifest.pageUrl);
    });
    cdp = await Cdp.connect(page.webSocketDebuggerUrl);
    await cdp.send("Runtime.enable");
    await cdp.send("Page.enable");
    const appInfo = await evaluate("window.octopusTransport.appInfo()");
    if (!appInfo.isPackaged) throw failure("The renderer is not a packaged app.", EXIT.launch);
    mark("owned packaged renderer", { appInfo, pageUrl: page.url });
    enterBoundary("welcome", EXIT.welcome);
    for (const step of ["hero", "preview", "manifesto", "primitives"]) {
      await waitFor(`welcome ${step}`, () => has(`main.welcome[data-step="${step}"]`));
      await waitFor(`welcome ${step} ready`, () => evaluate("(() => {const f=document.querySelector('.welcome-footer');return f&&f.dataset.hidden==='false';})()"));
      await snapshot(`welcome-${step}`);
      await click(".welcome-footer .welcome-next");
      mark(`welcome ${step}`);
    }
    enterBoundary("sign-in", EXIT.signIn);
    if (manifest.mode === "unauthenticated") {
      await waitFor("sign-in action", () => has(".welcome-signin:not(:disabled)"));
      const account = await evaluate("window.octopusTransport.chatgptAccount()");
      if (account.signedIn) throw failure("The unauthenticated environment unexpectedly has an account.", EXIT.signIn);
      await snapshot("sign-in-action");
      await click(".welcome-signin");
      const loginState = await waitFor("native sign-in launch", async () => {
        if ((await body()).includes("Finish signing in") && await has(".welcome-signin:disabled")) return "pending";
        if (await has(".launch-scene") && (await evaluate("window.octopusTransport.chatgptAccount()")).signedIn) return "completed";
        return null;
      });
      await snapshot("sign-in-action-result");
      mark("native sign-in action opened browser", { accountSignedIn: loginState === "completed", loginState, scope: loginState === "pending" ? "sign-in action only; authentication completion is not claimed" : "native sign-in completed; runtime onboarding is not claimed" });
      if (loginState === "pending") {
        await click("button", "Cancel");
        await waitFor("sign-in cancelled", () => has(".welcome-signin:not(:disabled)"));
        mark("native sign-in cancelled in client");
      }
      report.status = "passed";
      report.boundary = "sign-in";
      report.exitCode = EXIT.passed;
      save();
      return;
    }
    await waitFor("reused credential accepted", () => has(".launch-scene"));
    const account = await evaluate("window.octopusTransport.chatgptAccount()");
    if (!account.signedIn) throw failure("The explicitly selected credential was not accepted by the installed provider.", EXIT.signIn);
    mark("selected credential accepted", { accountSignedIn: true, reusedCredentialOnly: true });
    enterBoundary("boot", EXIT.boot);
    await waitFor("launch terminal", () => has(".launch-scene[data-phase=terminal] .xterm-helper-textarea"));
    const beforeBoot = await evaluate("window.octopusTransport.bootStatus()");
    if (beforeBoot.ready) throw failure("The isolated Home was already booted before its launch action.", EXIT.boot);
    if (!beforeBoot.launchCommand?.includes(path.join(manifest.appRoot, "bin/octopus"))) throw failure("The launch action does not name the bundled runtime.", EXIT.boot);
    writeJson(path.join(manifest.reportRoot, "boot-before.json"), beforeBoot, redact);
    await snapshot("launch-terminal");
    await evaluate("document.querySelector('.xterm-helper-textarea').focus()");
    await key("Enter", "Enter", 13, "\r");
    await waitFor("runtime boot and password", () => has(".welcome-lock-form input[type=password]"), 180000);
    const afterBoot = await evaluate("window.octopusTransport.bootStatus()");
    if (!afterBoot.ready) throw failure("The client showed a password before the fresh runtime was ready.", EXIT.boot);
    writeJson(path.join(manifest.reportRoot, "boot-after.json"), afterBoot, redact);
    try { await captureRuntimeEvidence("boot"); } catch (error) { report.runtimeCaptureError = redact(error.message); }
    mark("bundled runtime booted", { launchCommand: beforeBoot.launchCommand });
    enterBoundary("password", EXIT.password);
    if (!(await has('input[autocomplete="new-password"]'))) throw failure("The fresh client asked to unlock an existing password.", EXIT.password);
    await snapshot("set-password");
    await type(".welcome-lock-form input[type=password]", password);
    await waitFor("set password enabled", () => has(".welcome-lock-go:not(:disabled)"));
    await click(".welcome-lock-go");
    const afterPassword = await waitFor("workspace or Touch ID offer", async () => {
      if (await has(".setup-step")) return "workspace";
      return (await body()).includes("Use Touch ID next time") ? "touch-id" : null;
    });
    if (afterPassword === "touch-id") { await snapshot("touch-id-offer"); await click("button", "Not now"); }
    await waitFor("workspace setup", () => has(".setup-step"));
    mark("fresh local password configured");
    enterBoundary("setup", EXIT.setup);
    for (const [step, title] of [["space", "How will you use Octopus?"], ["projects", "Name the areas you care about"], ["capabilities", "Choose what Octopus can use"], ["review", "Ready to set up"]]) {
      await waitFor(`setup ${step}`, () => evaluate(`document.querySelector('.setup-step h1')?.textContent===${JSON.stringify(title)}`));
      if (step === "space") {
        const workSelector = ".setup-presets button";
        const workPoint = await evaluate("(() => {const e=[...document.querySelectorAll('.setup-presets button')].find(e=>e.querySelector('.welcome-card-title')?.textContent==='Work');if(!e)return null;const r=e.getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2};})()");
        if (!workPoint) throw failure(`Missing Work preset: ${workSelector}`, EXIT.setup);
        await cdp.send("Input.dispatchMouseEvent", { type: "mousePressed", button: "left", clickCount: 1, ...workPoint });
        await cdp.send("Input.dispatchMouseEvent", { type: "mouseReleased", button: "left", clickCount: 1, ...workPoint });
      }
      await waitFor(`${step} continue enabled`, () => has(".welcome-footer .welcome-next:not(:disabled)"), 120000);
      const evidence = await snapshot(`setup-${step}`);
      mark(`workspace ${step}`, { screenshot: evidence.screenshot });
      await click(".welcome-footer .welcome-next");
    }
    await waitFor("client setup complete", async () => {
      const text = await body();
      if (await has(".setup-error,[data-tone=danger]")) throw failure(`Client setup failed: ${text}`, EXIT.setup);
      if (/Needs attention/.test(text)) throw failure(`Client setup needs attention: ${text}`, EXIT.setup);
      if (await has(".welcome-signin")) throw failure("Workspace setup unexpectedly requested another sign-in.", EXIT.setup);
      if (await has("main.welcome")) return false;
      const evidence = await readSetupEvidence();
      if (evidence.setup?.status !== "complete" || evidence.workspace?.status !== "configured") return false;
      report.workspace = evidence.workspace;
      return true;
    }, 300000);
    mark("client setup receipt verified");
    enterBoundary("home", EXIT.home);
    await waitFor("Home composer", () => has(".home-response-stage__composer .ProseMirror[contenteditable=true]"));
    const defaults = await evaluate("window.octopusTransport.defaults()");
    if (defaults.octopusHome !== manifest.home || defaults.registryRoot !== path.join(manifest.home, "registry")) throw failure("Home projected a different data boundary.", EXIT.home);
    await snapshot("home-ready");
    mark("fresh Home ready", { defaults, ...ledgerEvidence("home-ready", false) });
    enterBoundary("home-reply", EXIT.homeReply);
    const nonce = "ONBOARDING_HOME_" + crypto.randomBytes(6).toString("hex");
    await type(".home-response-stage__composer .ProseMirror[contenteditable=true]", `Compute 19 + 23. Start your reply with ${nonce}: followed by the numeric result. Do not use tools or change configuration.`);
    await waitFor("Home send enabled", () => has(".home-response-stage__composer .composer-primary-action:not(:disabled):not([aria-disabled=true])"));
    await click(".home-response-stage__composer .composer-primary-action");
    await waitFor("actual Home reply", async () => {
      if (await has(".home-response-failure")) throw failure(await body(), EXIT.homeReply);
      await assertNoDeliveryFailure(".home-response-stage__composer");
      const reply = await evaluate("document.querySelector('.home-response-output__copy')?.innerText||''");
      if (!new RegExp(`${nonce}:\\s*42(?:\\s|$)`).test(reply) || await has(".home-response-output--streaming")) return false;
      report.homeReply = { promptNonce: nonce, reply };
      return true;
    }, 300000);
    await snapshot("home-reply");
    mark("actual Home reply received", ledgerEvidence("home-reply"));
    if (manifest.target === "task") {
      enterBoundary("task", EXIT.task);
      await click('[data-navigation-destination="tasks"]');
      const agent = await createTaskAgent();
      const previousRunIds = new Set((await projection("projection.operator.chats.list")).chats.map((run) => run.run_id));
      await waitFor("new task action", () => has('button[aria-label^="New task"]'));
      await click('button[aria-label^="New task"]');
      await waitFor("ordinary task composer", () => has(".chat-conversation-pane .ProseMirror[contenteditable=true]"));
      const projectSelector = '.chat-conversation-pane button[data-composer-navigation-target="workspace-project"]';
      await waitFor("ordinary task Project selector", () => has(`${projectSelector}:not(:disabled):not([aria-disabled=true])`));
      await click(projectSelector);
      const projectOption = `[role="menu"] button[role="menuitem"][title=${JSON.stringify(agent.projectName)}]`;
      await waitFor("created Agent Project choice", () => has(projectOption));
      await snapshot("ordinary-task-project-choices");
      await click(projectOption);
      await waitFor("created Agent Project selected for task", () => evaluate(`document.querySelector(${JSON.stringify(projectSelector)})?.innerText.trim()===${JSON.stringify(agent.projectName)}`));
      mark("ordinary task Project explicitly selected", { projectEeId: agent.projectEeId, projectName: agent.projectName });
      await waitFor("ordinary task Agent selector", () => has('.chat-conversation-pane button[data-composer-navigation-target="workspace-agent"]:not(:disabled)'));
      await click('.chat-conversation-pane button[data-composer-navigation-target="workspace-agent"]');
      const agentOption = `[role="menu"] button[role="menuitem"][title=${JSON.stringify(agent.name)}]`;
      await waitFor("created Agent choice", () => has(agentOption));
      await click(agentOption);
      await waitFor("created Agent selected for task", () => evaluate(`document.querySelector('.chat-conversation-pane button[data-composer-navigation-target="workspace-agent"]')?.title===${JSON.stringify(`Agent: ${agent.name}`)}`));
      mark("ordinary task Agent explicitly selected", { agentId: agent.id, name: agent.name });
      await snapshot("ordinary-task-ready");
      const taskNonce = "ONBOARDING_TASK_" + crypto.randomBytes(6).toString("hex");
      await type(".chat-conversation-pane .ProseMirror[contenteditable=true]", `Compute 7 * 8. Start your reply with ${taskNonce}: followed by the numeric result. Do not use tools or change configuration.`);
      await waitFor("ordinary task send enabled", () => has(".chat-conversation-pane .composer-primary-action:not(:disabled):not([aria-disabled=true])"));
      await click(".chat-conversation-pane .composer-primary-action");
      await waitFor("actual ordinary task reply", async () => {
        await assertNoDeliveryFailure(".chat-conversation-pane");
        const text = await evaluate("[...document.querySelectorAll('.chat-conversation-pane .octopus-markdown-root')].filter(e=>!e.closest('[data-thread-user-message-id]')).map(e=>e.innerText).join('\\n')");
        if (!new RegExp(`${taskNonce}:\\s*56(?:\\s|$)`).test(text) || await has(".chat-conversation-pane .octopus-streaming-markdown")) return false;
        report.taskReply = { promptNonce: taskNonce, reply: text };
        return true;
      }, 300000);
      const taskRuns = (await projection("projection.operator.chats.list")).chats.filter((run) => run.client_surface_role === "task" && !previousRunIds.has(run.run_id));
      if (taskRuns.length !== 1 || taskRuns[0].ee_id !== agent.projectEeId || taskRuns[0].worker_choice_interface_id !== agent.workerInterfaceId || taskRuns[0].provider_interface_id !== agent.providerInterfaceId) throw failure("The actual task run is not bound to the Agent selected through UI.", EXIT.task);
      writeJson(path.join(manifest.reportRoot, "ordinary-task-run.json"), taskRuns[0], redact);
      report.taskReply.runId = taskRuns[0].run_id;
      await snapshot("ordinary-task-reply");
      mark("actual ordinary task reply received", ledgerEvidence("ordinary-task-reply"));
    }
    report.status = "passed";
    report.boundary = manifest.target;
    report.exitCode = EXIT.passed;
    save();
  } catch (error) {
    report.status = "failed";
    report.exitCode = error.exitCode || boundaryCode;
    report.error = redact(error.safeMessage || error.message || String(error));
    if (cdp) {
      try { const captured = await snapshot("failure"); report.failureEvidence = { screenshot: captured.screenshot, screenText: captured.state.text, alerts: captured.state.alerts }; } catch (captureError) { report.captureError = redact(captureError.message); }
      try {
        const evidence = await readSetupEvidence();
        if (evidence.setup?.lastError) report.setupLastError = evidence.setup.lastError;
      } catch (readError) { report.setupReadError = redact(readError.message); }
      writeJson(path.join(manifest.reportRoot, "renderer-errors.json"), cdp.events, redact);
      try { await captureRuntimeEvidence("failure"); } catch (captureError) { report.runtimeCaptureError = redact(captureError.message); }
      const clientLog = path.join(manifest.home, "client/logs/octopus-client.jsonl");
      if (fs.existsSync(clientLog)) fs.writeFileSync(path.join(manifest.reportRoot, "failure-client.log"), redact(fs.readFileSync(clientLog, "utf8").slice(-64000)), { mode: 0o600 });
    }
    save();
    console.error(`${report.boundary}: ${report.error}`);
    process.exitCode = report.exitCode;
  } finally { cdp?.close(); }
}

walk(process.argv.slice(2)).catch((error) => {
  console.error(error.safeMessage || "The packaged UI walk could not start; inspect its private report.");
  process.exitCode = error.exitCode || EXIT.launch;
});
