const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const { createHash } = require('node:crypto');
const { Writable } = require('node:stream');
const ts = require('typescript');
const React = require('react');
const { renderToPipeableStream, renderToStaticMarkup } = require('react-dom/server');
const { JSDOM } = require('jsdom');
const app = path.resolve(__dirname, '..');
const read = name => fs.readFileSync(path.join(app, name), 'utf8');
const notices = JSON.parse(read('licenses/icon-assets/notices.json'));
const hash = value => createHash('sha256').update(value).digest('hex');
const modules = new Map(), registrations = [];
function load(file) {
  if (modules.has(file)) return modules.get(file).exports;
  const result = { exports: {} };
  modules.set(file, result);
  const compiled = ts.transpileModule(read(file), {
    compilerOptions: { jsx: ts.JsxEmit.ReactJSX, module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText;
  vm.runInNewContext(compiled, { module: result, exports: result.exports, require: id => id.startsWith('.')
    ? load(path.join(path.dirname(file), id) + '.tsx') : require(id) });
  return result.exports;
}
const glyphs = load('src/components/IconGlyphs.tsx');
const register = glyphs.registerGlyphPack;
glyphs.registerGlyphPack = (owner, pack) => { registrations.push({ owner, pack }); register(owner, pack); };
assert.equal(modules.size, 1, 'Importing the shared glyph set must not load a glyph pack');
const composer = ts.createSourceFile('ChatComposer.tsx', read('src/components/chat/ChatComposer.tsx'), ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
let glyphAttributes;
function visit(node) {
  if (ts.isJsxSelfClosingElement(node) && node.tagName.getText(composer) === 'Glyph'
    && node.attributes.getText(composer).includes('resumeActionAvailable')) glyphAttributes = node.attributes.properties;
  ts.forEachChild(node, visit);
}
visit(composer);
assert.ok(glyphAttributes, 'Composer must render a glyph for its Resume action');
const props = Object.fromEntries(glyphAttributes.map(attribute => [attribute.name.text,
  vm.runInNewContext(attribute.initializer.expression.getText(composer), {
    stopAvailable: false, resumeActionAvailable: true, isSubmitting: false, submissionMode: 'send',
  }),
]));
assert.equal(props.name, 'resume');
assert.equal(props.size, 20);
assert.equal(props.className, 'icon-primary-action');
async function verify() {
  const html = await new Promise((resolve, reject) => {
    let output = '';
    const sink = new Writable({ write(chunk, encoding, done) { output += chunk.toString(); done(); } });
    sink.on('finish', () => resolve(output)); sink.on('error', reject);
    const stream = renderToPipeableStream(React.createElement(glyphs.Glyph, props), {
      onAllReady() { stream.pipe(sink); }, onError: reject,
    });
  });
  assert.deepEqual([...modules.keys()], ['src/components/IconGlyphs.tsx', 'src/components/icon-glyphs/chat.tsx', 'src/components/icon-glyphs/licensed-paths.tsx', 'src/components/icon-glyphs/geometric-primitives.tsx']);
  assert.equal(registrations.length, 1);
  assert.equal(registrations[0].owner, 'chat');
  for (const name of ['send', 'stop', 'resume']) assert.ok(registrations[0].pack[name], `${name} belongs to the same lazy chat pack`);
  assert.throws(() => register('worker', { resume: registrations[0].pack.resume }), /registered by more than one pack/);
  const dom = new JSDOM(html);
  const svg = dom.window.document.querySelector('svg');
  assert.equal(svg.getAttribute('viewBox'), '0 0 20 20');
  assert.equal(svg.querySelector('path').getAttribute('fill'), 'currentColor');
  assert.equal(svg.querySelectorAll('path').length, 1);
  assert.equal(svg.getAttribute('width'), '20');
  assert.equal(svg.getAttribute('height'), '20');
  assert.equal(svg.getAttribute('aria-hidden'), 'true');
  assert.equal(svg.getAttribute('focusable'), 'false');
  dom.window.close();
  verifyAssets();
  console.log('Glyph registration, licensed sources, lazy Resume registration, selected states and accessibility passed.');
}

function verifyAssets() {
  assert.equal(notices.schema, 'octopus.client_icon_notices.v1');
  for (const [id, upstream] of Object.entries(notices.upstreams)) {
    assert.match(upstream.commit, /^[a-f0-9]{40}$/);
    assert.equal(hash(read('licenses/icon-assets/' + upstream.notice)), upstream.notice_sha256, id + ': notice bytes');
  }
  for (const pack of ['shell', 'navigation', 'worker', 'voice', 'settings']) load('src/components/icon-glyphs/' + pack + '.tsx');
  const registered = Object.assign({}, ...registrations.map(({ pack }) => pack));
  const glyphFile = path.join(app, 'src/components/IconGlyphs.tsx');
  const program = ts.createProgram([glyphFile], { jsx: ts.JsxEmit.ReactJSX, noEmit: true, noResolve: true, types: [] });
  const glyphName = program.getSourceFile(glyphFile).statements.find(node => ts.isTypeAliasDeclaration(node) && node.name.text === 'GlyphName');
  const apiNames = program.getTypeChecker().getTypeAtLocation(glyphName.name).types.map(type => type.value).sort();
  assert.deepEqual(Object.keys(registered).sort(), apiNames, 'Every GlyphName registers once');
  assert.equal(registrations.length, 6, 'Six lazy pack owners remain');
  for (const name of apiNames) {
    const dom = new JSDOM(renderToStaticMarkup(React.createElement(glyphs.Glyph, { name, size: 19, className: 'asset-check' })));
    const svg = dom.window.document.querySelector('svg');
    for (const [attribute, value] of Object.entries({width:'19',height:'19','aria-hidden':'true',focusable:'false'})) assert.equal(svg.getAttribute(attribute), value);
    assert.ok(svg.classList.contains('asset-check'));
    dom.window.close();
  }
  const usedShapes = new Set();
  const licensed = load('src/components/icon-glyphs/licensed-paths.tsx');
  for (const entry of notices.glyphs) {
    const dom = new JSDOM(renderToStaticMarkup(React.createElement(glyphs.Glyph, { name: entry.name, size: 19 })));
    const svg = dom.window.document.querySelector('svg');
    for (const name of entry.shapes) {
      usedShapes.add(name); const shape = notices.shapes[name];
      const upstreamSource = read('licenses/icon-assets/' + shape.path);
      assert.equal(hash(upstreamSource), shape.sha256);
      assert.ok(shape.source_url.includes(notices.upstreams[shape.upstream].commit));
      const upstream = new JSDOM(upstreamSource, {contentType: shape.path.endsWith('.svg') ? 'image/svg+xml' : 'text/html'});
      if (shape.path.endsWith('.svg')) for (const primitive of upstream.window.document.documentElement.children) {
        assert.ok([...svg.querySelectorAll(primitive.localName)].some(node => [...primitive.attributes].every(attr => node.getAttribute(attr.name) === attr.value)), entry.name + ': upstream primitive');
      } else for (const [,d] of upstreamSource.matchAll(/d="([^"]+)"/g)) {
        if (entry.translation) assert.equal(licensed.PLAY_SM_PATH, d);
        const expected = entry.translation ? licensed.translatedAbsoluteCubicPath(d, entry.translation.dx, entry.translation.dy, entry.translation.significant_digits) : d;
        assert.ok([...svg.querySelectorAll('path')].some(node => node.getAttribute('d') === expected));
      }
      upstream.window.close();
    }
    dom.window.close();
  }
  assert.deepEqual([...usedShapes].sort(), Object.keys(notices.shapes).sort(), 'Only active licensed shapes are vendored');
  for (const name of ['home','tasks','dashboardRail','inbox','applicationsRail','driversRail']) assert.notEqual(renderToStaticMarkup(registered[name].node), renderToStaticMarkup(registered[name+'Filled'].node), name + ': selected state distinct');
  for (const name of ['resume','play','stop']) assert.match(renderToStaticMarkup(registered[name].node), /fill="currentColor"/);
  const { metafile } = require('esbuild').buildSync({ entryPoints: ['src/components/worker-environment/RunEnvironmentBrowserSurfaces.tsx'], absWorkingDir: app, bundle: true, write: false, metafile: true, outdir: 'browser-surface', logLevel: 'silent',
    loader: { '.css': 'empty', '.svg': 'dataurl', '.png': 'file' }, external: ['react', 'react-dom', 'react/jsx-runtime', 'motion/react', '@pierre/diffs', '@pierre/diffs/*', 'shiki', '@shikijs/*', 'mermaid', 'katex'] });
  assert.ok(metafile.inputs['src/assets/browser-cursor.png'], 'The browser surface draws the model cursor from the shipped cursor asset');
  const primitives = load('src/components/icon-glyphs/geometric-primitives.tsx');
  assert.notEqual(primitives.roundedCrossPath(10,10,6.5), primitives.roundedCrossPath(15,15,4), 'Cross parameters control dimensions');
  assert.notEqual(primitives.cubicCirclePath(10,10,7,3.8), primitives.cubicCirclePath(12,12,4,2.2), 'Circle parameters control dimensions');
}
verify().catch(error => { console.error(error); process.exitCode = 1; });
