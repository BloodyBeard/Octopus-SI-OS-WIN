const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { createRequire } = require('node:module');
const { spawnSync } = require('node:child_process');
const test = require('node:test');
const { createHash } = require('node:crypto');

const root = path.resolve(__dirname, '../../../..');
const read = relative => JSON.parse(fs.readFileSync(path.join(root, relative), 'utf8'));
const artifact = id => read(`registry/artifact/${id}/artifact.json`);
const policy = read('registry/interface/system_worker_operator_home/policy.json');
const example = JSON.parse(fs.readFileSync(path.join(__dirname, 'fixtures/home-task-example.json'), 'utf8'));
const Ajv = createRequire(path.join(root, 'app/package.json'))('ajv');
const ajv = new Ajv({ strict: false, formats: { uint64: true } });
const schema = artifact('artifact-home-task-authoring').run_record_schema;
const validate = ajv.compile(schema.document);

function verify(id, request) {
  const result = spawnSync('python3', [path.join(root, `registry/verifier/${id}/verify.py`)], {
    input: JSON.stringify(request), encoding: 'utf8',
  });
  assert.equal(result.status, 0, result.stderr);
  return JSON.parse(result.stdout);
}

test('Home bootstrap exposes declared authoring tools and readable role closure', () => {
  const source = fs.readFileSync(path.join(root, schema.source));
  assert.equal(schema.hash_algorithm, 'sha256');
  assert.equal(schema.source_hash, createHash('sha256').update(source).digest('hex'));
  assert.deepEqual(schema.document, JSON.parse(source));
  const pack = read('app/distribution/local-packages/packs/codex.json');
  const home = pack.workspace_setup.home;
  assert.equal(home.worker_interface_id, policy.id);
  assert.ok(home.selected_worker_environment_kinds.includes('system_room'));
  // Home runs pin the worker's role artifacts themselves (chat_run_record.rs),
  // so the pack only has to install every one of them.
  const installed = new Set(pack.components.map(component => component.package_id));
  const packages = fs.readdirSync(path.join(root, 'app/distribution/local-packages/packages'))
    .map(name => read(`app/distribution/local-packages/packages/${name}`));
  for (const id of policy.role_artifacts) {
    assert.equal(artifact(id).label, 'context.reference');
    assert.ok(artifact(id).text.trim());
    const owners = packages.filter(entry =>
      entry.members.some(member => member.registry_kind === 'artifact' && member.owner === id));
    assert.equal(owners.length, 1, `${id} has exactly one owning package`);
    assert.ok(installed.has(owners[0].id), `${id} is installed by the setup pack`);
  }
  for (const id of ['file.read', 'file.list', 'file.write', 'artifact.send', 'artifact.read', 'artifact.list', 'artifact.search']) {
    const tool = read(`registry/tool/${id}/tool.json`);
    assert.ok(policy.ceiling.tools.includes(id), id);
    assert.ok(policy.ceiling.interfaces.includes(tool.interface), tool.interface);
    assert.deepEqual(tool.supported_worker_environment_kinds, ['system_room']);
  }
});

test('authored example has independent operator, assignment and result edges', () => {
  const records = [example.home_record, example.task_record];
  for (const record of records) {
    assert.ok(validate(record), JSON.stringify(validate.errors));
    const worker = example.worker_policies.find(pin => pin.id === record.worker);
    for (const cp of record.checkpoints) {
      assert.ok(record.active_checkpoints.includes(cp.id));
      assert.equal(cp.run, record.id);
      for (const binding of [cp.interface_binding, ...cp.outbound_interface_bindings].filter(Boolean)) {
        assert.ok(worker.ceiling.interfaces.includes(binding.interface));
        assert.ok(example.ee_template.allowed_interfaces.includes(binding.interface));
      }
      for (const role of worker.role_artifacts) assert.ok(cp.artifact_bindings.some(binding => binding.id === role));
    }
  }
  const input = example.home_record.checkpoints.find(cp => cp.id === 'home_input');
  const outcome = example.home_record.checkpoints.find(cp => cp.id === 'home_result');
  assert.deepEqual(input.required_verified_artifacts, []);
  assert.equal(outcome.interface_binding.interface, example.return_tool_arguments.interface);
  assert.equal(example.task_record.worker, example.assignment_tool_arguments.interface);
  assert.equal(outcome.required_verified_artifacts[0].artifact_label, example.return_tool_arguments.label);
  assert.notEqual(input.id, outcome.id);
  const malformed = structuredClone(example.task_record);
  malformed.checkpoints[0].interface_binding = malformed.worker;
  assert.equal(validate(malformed), false);
});

test('real verifier gates reject undeclared calls and prove receipt without asserting success', () => {
  const requirement = example.task_record.checkpoints.find(cp => cp.id === 'task_tools').required_verified_artifacts[0];
  const payload = { tool: 'artifact.send', arguments: example.return_tool_arguments };
  const request = { params: requirement.params, payload_utf8: JSON.stringify(payload) };
  assert.equal(verify(requirement.verifier, request).passed, true);
  assert.equal(verify(requirement.verifier, { ...request, payload_utf8: JSON.stringify({ ...payload, tool: 'file.write' }) }).passed, false);
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'octopus-home-contract-'));
  try {
    const payloadPath = path.join(temporary, 'outcome.json');
    fs.writeFileSync(payloadPath, JSON.stringify(example.return_tool_arguments.payload));
    const receipt = example.home_record.checkpoints.find(cp => cp.id === 'home_result').required_verified_artifacts[0];
    assert.equal(receipt.mark, 'task.summary.received');
    assert.ok(policy.ceiling.mark_outcomes.includes(receipt.mark));
    assert.equal(example.return_tool_arguments.payload.status, 'blocked');
    assert.equal(verify(receipt.verifier, { payload_path: payloadPath }).passed, true);
    assert.equal(verify(receipt.verifier, { payload_path: path.join(temporary, 'missing') }).passed, false);
  } finally {
    fs.rmSync(temporary, { recursive: true, force: true });
  }
});
