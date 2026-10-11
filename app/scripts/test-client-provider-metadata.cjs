const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const vm = require("node:vm");
const { test } = require("node:test");
const { createChatgptSignIn } = require("../electron/chatgptSignIn.cjs");
const { createClientProviderMetadata, validateProviderModelSelection } = require("../electron/clientProviderMetadata.cjs");

test("desktop setup and paginated model discovery work without a Home, socket, or ledger", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-client-provider-"));
  const binary = path.join(root, "codex-fixture.cjs");
  const methods = path.join(root, "methods.jsonl");
  let opened = 0, accountHome = root;
  await fs.writeFile(binary, `#!${process.execPath}
const fs = require('node:fs');
const readline = require('node:readline');
let signedIn = false;
const row = model => ({model,displayName:model,description:'',defaultReasoningEffort:'high',supportedReasoningEfforts:[{reasoningEffort:'high'}]});
readline.createInterface({input:process.stdin}).on('line', line => {
  const {id, method, params} = JSON.parse(line);
  fs.appendFileSync(${JSON.stringify(methods)}, JSON.stringify(method)+'\\n');
  if (id === undefined) return;
  const result = method === 'account/read' ? {account:signedIn ? {type:'chatgpt'} : null}
    : method === 'account/login/start' ? {type:'chatgpt',loginId:'login',authUrl:'https://example.test/login'}
    : method === 'model/list' ? {data:[row(params.cursor ? 'second' : 'first')], nextCursor:params.cursor ? null : 'next'} : {};
  process.stdout.write(JSON.stringify({id,result})+'\\n');
  if (method === 'account/login/start') {
    signedIn = true;
    setTimeout(() => process.stdout.write(JSON.stringify({method:'account/login/completed',params:{loginId:'login',success:true}})+'\\n'), 10);
  }
});
`, { mode: 0o700 });
  const client = createChatgptSignIn({ codexBinary: () => binary, codexHome: () => accountHome,
    version: () => "test", openExternal: async () => { opened += 1; } });
  try {
    const folder = path.join(root, "registry", "interface", "provider");
    await fs.mkdir(folder, { recursive: true });
    await fs.writeFile(path.join(folder, "interface.json"), JSON.stringify({ id: "provider", interface_type: "world",
      role: "provider", interface_driver_ref: "codex_app_server", interface_driver_config: nativeConfig(), interface_policy: { variables: { models: { default: ["second"] } } } }));
    const service = createClientProviderMetadata({ registryRoot: () => path.join(root, "registry"), accountClient: () => client,
      reference: (kind, id, evidence) => ({ kind, id, evidence }) });
    const source = await fs.readFile(path.join(__dirname, "../electron/main.cjs"), "utf8");
    const ownerFactory = source.slice(source.indexOf("function createClientSetupOwners("), source.indexOf("function clientSetupPackageRoots("));
    const owner = vm.runInNewContext(`${ownerFactory}; createClientSetupOwners().provider`, { getClientProviderMetadata: () => service });
    const plan = { provider: { interface_id: "provider" } };
    assert.equal(await owner.probe(plan), null);
    await owner.apply(plan);
    assert.equal((await owner.probe(plan))[0].evidence.signed_in, true);
    await owner.apply(plan);
    assert.equal(opened, 1, "an existing sign-in must not reopen the browser");
    const catalog = await service.models({ providerInterfaceId: "provider" });
    assert.deepEqual(catalog.models.map((row) => row.model), ["second"]);
    assert.deepEqual(catalog.models[0].supportedReasoningEfforts, ["high"]);
    await assert.rejects(service.models({ providerInterfaceId: "../provider" }), /Invalid provider/);
    const calls = (await fs.readFile(methods, "utf8")).trim().split("\n").map(JSON.parse);
    assert.equal(calls.filter((method) => method === "model/list").length, 2);
    assert.ok(calls.every((method) => ["initialize", "initialized", "account/read", "account/login/start", "model/list"].includes(method)));
    accountHome = path.join(root, "another-account");
    assert.equal((await client.account()).signedIn, false, "a changed account Home must not reuse the previous app-server");
  } finally {
    client.dispose();
    await fs.rm(root, { recursive: true, force: true });
  }
});

test("account failures remain explicit and unsupported providers never reach Codex", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-client-provider-failure-"));
  try {
    const folder = path.join(root, "interface", "provider");
    await fs.mkdir(folder, { recursive: true });
    const descriptor = { id: "provider", interface_type: "world", role: "provider", interface_driver_ref: "other" };
    const write = () => fs.writeFile(path.join(folder, "interface.json"), JSON.stringify(descriptor));
    await write();
    const service = createClientProviderMetadata({ registryRoot: () => root, accountClient: () => ({
      account: async () => { throw new Error("account transport failed"); },
    }), reference: () => assert.fail("must not manufacture signed-in evidence") });
    const plan = { provider: { interface_id: "provider" } };
    await assert.rejects(service.setupOwner.probe(plan), /does not support/);
    descriptor.interface_driver_ref = "codex_app_server";
    descriptor.interface_driver_config = nativeConfig();
    await write();
    await assert.rejects(service.setupOwner.probe(plan), /account transport failed/);
    await assert.rejects(service.setupOwner.apply(plan), /account transport failed/);
  } finally {
    await fs.rm(root, { recursive: true, force: true });
  }
});

function nativeConfig() {
  return { residual_ambient: { private_auth_store: { auth_file_env: "OCTOPUS_MACHINE_CODEX_AUTH_FILE" } } };
}

test("catalogue shares native account only, preserves exact OpenCodex models, and rejects stale selections", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-catalogue-"));
  let selectedRoot = root, calls = 0;
  const row = model => ({ model, displayName: model, supportedReasoningEfforts: ["high"], defaultReasoningEffort: "high", showInPicker: true });
  const nativeModels = [row("live-model"), row("outside-policy")];
  const native = { id: "native", runtimeHandlerRef: "codex_app_server", models: [row("stale-model")] };
  const custom = { id: "custom", runtimeHandlerRef: "codex_app_server", models: [row("stale-custom-model")] };
  const other = { id: "other", runtimeHandlerRef: "other", models: [row("other-model")] };
  const write = async (id, config, allowed) => {
    const folder = path.join(root, "interface", id);
    await fs.mkdir(folder, { recursive: true });
    await fs.writeFile(path.join(folder, "interface.json"), JSON.stringify({ id, interface_type: "world", role: "provider",
      interface_driver_ref: "codex_app_server", interface_driver_config: config, interface_policy: { variables: { models: { default: allowed } } } }));
  };
  const customConfig = { private_config: { opencodex: { schema_version: 1, adapter: "openai-chat", credential_owner_interface: "custom", model_catalog: { models: [{
    slug: "custom-model", display_name: "Custom", description: "Custom transport", default_reasoning_level: "high", supported_reasoning_levels: [{ effort: "high" }], visibility: "list", supported_in_api: true,
  }] } } }, transport: { wire: "openai_chat" } };
  try {
    await write("native", nativeConfig(), ["live-model"]);
    await write("custom", customConfig, ["custom-model"]);
    const service = createClientProviderMetadata({ registryRoot: () => selectedRoot,
      accountClient: () => ({ models: async () => { calls++; return nativeModels; } }) });
    const providers = await service.catalog([native, custom, other]);
    assert.equal(calls, 1);
    assert.deepEqual(providers.map(provider => provider.models.map(model => model.model)), [["live-model"], ["custom-model"], ["other-model"]]);
    await service.catalog([custom]); assert.equal(calls, 1);
    await assert.rejects(service.setupOwner.probe({ provider: { interface_id: "custom" } }), /does not use the desktop/);
    validateProviderModelSelection(providers, { providerInterfaceId: "custom", providerModel: "custom-model", reasoningEffort: "high" });
    for (const selection of [{ providerInterfaceId: "native", providerModel: "custom-model", reasoningEffort: "high" },
      { providerInterfaceId: "native", providerModel: "stale-model", reasoningEffort: "high" },
      { providerInterfaceId: "native", providerModel: "live-model", reasoningEffort: "ultra" }]) {
      assert.throws(() => validateProviderModelSelection(providers, selection));
    }
    nativeModels.splice(0, 1);
    assert.match((await service.catalog([native]))[0].modelCatalogError, /no available declared models/);
    const scoped = createClientProviderMetadata({ registryRoot: () => selectedRoot, accountClient: () => ({ models: async () => { selectedRoot = "another-home"; return [row("live-model")]; } }) });
    await assert.rejects(scoped.catalog([native]), /scope changed/); selectedRoot = root;
    delete customConfig.private_config.opencodex.model_catalog;
    await write("custom", customConfig, ["custom-model"]);
    const unavailableCustom = await service.catalog([custom, other]);
    assert.match(unavailableCustom[0].modelCatalogError, /no valid OpenCodex/);
    assert.equal(unavailableCustom[0].models.length, 0);
    assert.equal(unavailableCustom[1], other);
    const failing = createClientProviderMetadata({ registryRoot: () => root, accountClient: () => ({ models: async () => { throw new Error("offline"); } }) });
    assert.match((await failing.catalog([native]))[0].modelCatalogError, /offline/);
  } finally { await fs.rm(root, { recursive: true, force: true }); }
});
