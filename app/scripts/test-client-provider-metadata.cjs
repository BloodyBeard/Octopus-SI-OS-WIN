const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const vm = require("node:vm");
const { test } = require("node:test");
const { createChatgptSignIn } = require("../electron/chatgptSignIn.cjs");
const { createClientProviderMetadata } = require("../electron/clientProviderMetadata.cjs");

test("desktop setup and paginated model discovery work without a Home, socket, or ledger", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "octopus-client-provider-"));
  const binary = path.join(root, "codex-fixture.cjs");
  const methods = path.join(root, "methods.jsonl");
  let opened = 0;
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
  const client = createChatgptSignIn({ codexBinary: () => binary, codexHome: () => root,
    version: () => "test", openExternal: async () => { opened += 1; } });
  try {
    const folder = path.join(root, "registry", "interface", "provider");
    await fs.mkdir(folder, { recursive: true });
    await fs.writeFile(path.join(folder, "interface.json"), JSON.stringify({ id: "provider", interface_type: "world",
      role: "provider", interface_driver_ref: "codex_app_server", interface_policy: { variables: { models: { default: ["second"] } } } }));
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
    await write();
    await assert.rejects(service.setupOwner.probe(plan), /account transport failed/);
    await assert.rejects(service.setupOwner.apply(plan), /account transport failed/);
  } finally {
    await fs.rm(root, { recursive: true, force: true });
  }
});
