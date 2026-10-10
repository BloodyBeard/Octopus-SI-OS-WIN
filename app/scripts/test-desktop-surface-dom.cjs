const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const ts = require('typescript');
const {test} = require('node:test');
const {JSDOM} = require('jsdom');
const React = require('react');
const {createRoot} = require('react-dom/client');
const rootPath=path.resolve(__dirname,'..');
const cache=new Map();
function load(relative) {
 const file=path.resolve(rootPath,relative); if(cache.has(file))return cache.get(file).exports;
 const module={exports:{}}; cache.set(file,module);
 const source=ts.transpileModule(fs.readFileSync(file,'utf8'),{fileName:file,compilerOptions:{target:ts.ScriptTarget.ES2022,module:ts.ModuleKind.CommonJS,jsx:ts.JsxEmit.ReactJSX,esModuleInterop:true}}).outputText;
 function local(specifier) {
  if(specifier.endsWith('.css'))return {};
  if(specifier.endsWith('design-system/components'))return {OcButton:({children,size,variant,...props})=>React.createElement('button',props,children),OcSelect:({options,label,onChange,placeholder,...props})=>React.createElement('select',{...props,'aria-label':label,onChange:e=>onChange(e.target.value)},React.createElement('option',{value:''},placeholder),options.map(o=>React.createElement('option',{key:o.value,value:o.value},o.label)))};
  if(specifier.endsWith('IconGlyphs'))return {Glyph:()=>null};
  if(specifier.endsWith('OctopusSurface'))return {OctopusNoteLine:({children})=>React.createElement('p',null,children)};
  if(specifier.endsWith('RunEnvironmentSurfacePrimitives'))return {LiveSurfaceEmpty:({title,detail})=>React.createElement('div',null,title,detail),dataText:()=>'',ManualRoomControlWarning:({open,onContinue})=>open?React.createElement('button',{onClick:onContinue},'Confirm control'):null};
  if(!specifier.startsWith('.'))return require(specifier);
  const target=path.resolve(path.dirname(file),specifier); for(const suffix of ['', '.ts','.tsx'])if(fs.existsSync(target+suffix))return load(target+suffix);
  throw new Error(`Unknown test import ${specifier}`);
 }
 new Function('require','module','exports',source)(local,module,module.exports);return module.exports;
}
const {DesktopSurface}=load('src/components/worker-environment/DesktopSurface.tsx');
const screen={id:'left',x:-960,y:0,width:960,height:540,pixel_width:1920,pixel_height:1080};
const image={media_type:'image/png',data_base64:'aW1hZ2U=',sha256:'a'.repeat(64),byte_length:5,width:1920,height:1080};
const evidence=(owner)=>({status:'ready',permissions:{screen_capture:true,accessibility:true},control:{mode:'model',owner,generation:1},screens:[screen],observation:{id:'first',screen,input_revision:'first',captured_at_unix_ms:1,focus:{app_id:'example.first',process_id:1,window_id:1},image}});

test('actual desktop component keeps evidence and busy state scoped to attachment, not pixel digest',async()=>{
 const dom=new JSDOM('<div id="root"></div>',{url:'http://localhost'});
 const w=dom.window; for(const key of ['window','document','HTMLElement','Event','MouseEvent'])Object.defineProperty(globalThis,key,{configurable:true,value:key==='window'?w:w[key]});
 globalThis.IS_REACT_ACT_ENVIRONMENT=true;
 const calls=[]; let value=evidence('room-a'); let fail=false; let pendingControl;
 w.octopusTransport={desktopSurfaceRequest:async request=>{
  calls.push(request);
  if(request.operation==='pause_view')return { ...request,evidence:value };
  if(request.operation!=='view')return new Promise(resolve=>{pendingControl=resolve;});
  if(fail)throw new Error('stream disconnected');
  return {...request,evidence:request.screenId?structuredClone(value):{...structuredClone(value),observation:null}};
 }};
 const root=createRoot(w.document.querySelector('#root'));
 let environment={id:'room-a',runId:'run-a'}; const surface={id:'desktop',mode:'direct',viewerUrl:'http://localhost/view',readOnly:false};
 const render=async(active=true)=>React.act(async()=>root.render(React.createElement(DesktopSurface,{environment,surface,active})));
 const tick=async()=>React.act(async()=>new Promise(resolve=>setTimeout(resolve,85)));
 const text=()=>w.document.body.textContent; const button=label=>[...w.document.querySelectorAll('button')].find(b=>b.textContent===label);
 const click=async label=>React.act(async()=>button(label).dispatchEvent(new w.MouseEvent('click',{bubbles:true})));
 try {
  await render();await tick(); assert(w.document.querySelector('img')); assert.match(text(),/Worker has control/);
  value={...evidence('room-a'),control:{mode:'human',owner:null,generation:2},permissions:{screen_capture:true,accessibility:false}};
  value.observation.focus={app_id:'example.changed',process_id:2,window_id:3};
  value.view_image_ref={sha256:image.sha256};value.observation.image=null;
  await tick();assert.match(text(),/You have control/);assert.match(text(),/Accessibility required/);assert.match(text(),/example.changed/);assert(w.document.querySelector('img'));
  value={...value,view_image_ref:undefined,observation:null};await tick();assert.equal(w.document.querySelector('img'),null);
  value=evidence('room-b');environment={id:'room-b',runId:'run-b'};await render();await tick();assert(w.document.querySelector('img'));assert.match(text(),/Worker has control/);
  await click('Stop computer use');assert(button('Refresh').disabled);assert(pendingControl);
  value=evidence('room-c');environment={id:'room-c',runId:'run-c'};await render();await tick();assert(!button('Refresh').disabled);
  await React.act(async()=>pendingControl({evidence:{...evidence('room-b'),control:{mode:'stopped',owner:null,generation:99}}}));
  assert.match(text(),/Worker has control/);assert(!button('Refresh').disabled);
  await click('Stop computer use');await render(false);
  await React.act(async()=>pendingControl({evidence:evidence('room-c')}));await render();await tick();assert(!button('Refresh').disabled);assert(w.document.querySelector('img'));
  fail=true;await tick();assert.equal(w.document.querySelector('img'),null);assert.match(text(),/stream disconnected/);
  const stopped={...evidence('room-c'),observation:null,control:{mode:'stopped',owner:null,generation:3}};
  const viewCalls=calls.filter(c=>c.operation==='view').length;
  assert(!button('Stop computer use').disabled);await click('Stop computer use');
  await React.act(async()=>pendingControl({evidence:stopped}));
  assert.match(text(),/Computer use stopped/);assert.equal(calls.filter(c=>c.operation==='view').length,viewCalls);
  await click('Release control');const release=pendingControl;
  assert.equal(calls.at(-1).controlGeneration,3);assert(!button('Stop computer use').disabled);
  await click('Stop computer use');await React.act(async()=>pendingControl({evidence:{...stopped,control:{...stopped.control,generation:4}}}));
  await React.act(async()=>release({evidence:{...stopped,control:{mode:'idle',owner:null,generation:3}}}));
  assert.match(text(),/Computer use stopped/);assert(!button('Refresh').disabled);
  fail=false;await click('Refresh');await tick();assert(w.document.querySelector('img'));
  await React.act(async()=>w.document.querySelector('img').dispatchEvent(new w.Event('error')));
  assert.equal(w.document.querySelector('img'),null);assert.match(text(),/could not be decoded/);
  assert(!button('Stop computer use').disabled);await click('Stop computer use');
  await React.act(async()=>pendingControl({evidence:stopped}));assert.match(text(),/Computer use stopped/);
  await React.act(async()=>root.unmount());
  const pauses=calls.filter(c=>c.operation==='pause_view');assert(pauses.length>3);
  assert(calls.filter(c=>!['view','pause_view'].includes(c.operation)).every(c=>['stop','release_control'].includes(c.operation)));
  console.log('PASS same pixels across rooms, control/focus/permissions delta, observation withdrawal, old control completion, hide/show busy, error/new-stream clear, viewer-only detach');
 } finally {await React.act(async()=>root.unmount());dom.window.close();}
});
