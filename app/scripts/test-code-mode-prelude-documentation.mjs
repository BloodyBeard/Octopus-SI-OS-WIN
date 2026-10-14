import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

const read = path => JSON.parse(readFileSync(new URL(`../../${path}`, import.meta.url), 'utf8'));
const cuaSource = read('registry/artifact/artifact-code-mode-cua-prelude/artifact.json');
const browserSource = read('registry/artifact/artifact-code-mode-browser-prelude/artifact.json');
const evaluate = sources => import(`data:text/javascript;base64,${Buffer.from([
  'export const requests = [], output = []; export const text = value => output.push(JSON.parse(JSON.stringify(value)));',
  'const tools = new Proxy({}, {get: (_, name) => async args => {requests.push({name,args}); throw new Error("Unexpected nested request");}});',
  ...sources.map(([source, roles]) => `${source.module_source}\nexport const ${source.export_name} = ${source.export_factory}(${JSON.stringify(Object.fromEntries(roles.map(role => [role, {tool:source.tool_roles[role].tool,name:role}])))});`),
].join('\n')).toString('base64')}`);

test('canonical ESM documentation is exact, binding-filtered, isolated, and performs no nested requests', async () => {
  const prelude = await evaluate([[browserSource, Object.keys(browserSource.tool_roles)], [cuaSource, Object.keys(cuaSource.tool_roles)]]);
  const { text } = prelude; text(await prelude.browser.documentation()); text(await prelude.cua.documentation());
  assert.equal(prelude.output.length, 2); assert.ok(Object.isFrozen(prelude.cua));
  const docs = prelude.output[1]; assert.deepEqual(docs.methods, Object.keys(prelude.cua));
  assert.match(docs.signatures.getApp, /kind:'bundle_id'\|'bundle_path',value:string/);
  assert.match(docs.app_signatures.getAXState, /rendered AX string/); assert.match(docs.app_signatures.getScreenshot, /Uint8Array/);
  for (const [roles, app] of [[['apps'], []], [['app_click'], []], [['app_state','app_click'], ['click']]]) {
    const fixture = await evaluate([[cuaSource, roles]]), info = fixture.cua.documentation();
    assert.deepEqual(info.methods, Object.keys(fixture.cua)); assert.deepEqual(info.declared_roles, [...roles].sort());
    assert.deepEqual(Object.keys(info.app_arguments), app); assert.deepEqual(Object.keys(info.signatures).sort(), [...info.methods].sort());
    if (!roles.includes('app_state')) assert.deepEqual(info.app, []);
    else {
      const fields = Object.keys(read('registry/tool/gui.app_click/tool.json').args_schema.properties).filter(key => !['grant_request_id','app','observation_id','revision','control_generation'].includes(key));
      assert.deepEqual([...info.app_arguments.click].sort(), fields.sort()); assert.equal(info.app.includes('scroll'), false);
      info.app_arguments.click.push('invented'); info.app.push('invented'); assert.equal(fixture.cua.documentation().app_arguments.click.includes('invented'), false);
      for (const [selector, options] of [['Fixture', {}], [{kind:'name',value:'Fixture'}, {}], [{kind:'bundle_id',value:'fixture',extra:true}, {}], [{kind:'bundle_id',value:'fixture'}, {disableDiffing:true}]]) await assert.rejects(fixture.cua.getApp(selector, options), TypeError);
    }
    assert.deepEqual(fixture.requests, []);
  }
  const browserOnly = await evaluate([[browserSource, ['tabs']]]); assert.equal(browserOnly.cua, undefined); assert.deepEqual(browserOnly.requests, []);
  assert.deepEqual(prelude.requests, []);
});
