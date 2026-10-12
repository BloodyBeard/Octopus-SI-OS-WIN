const assert = require('node:assert/strict');
const {test} = require('node:test');
const fs = require('node:fs');
const path = require('node:path');
const ts = require('typescript');
const source = fs.readFileSync(path.join(__dirname,'../src/components/chat/chatTurnControl.ts'),'utf8');
const compiled = ts.transpileModule(source,{compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2022}}).outputText;
const moduleExports = {};
Function('exports',compiled)(moduleExports);
const {reconcileChatRunReceipts:reconcile,operatorTurnResumeAcknowledgedOpen:resumeOpen}=moduleExports;
const idle={runId:'home',chatInterfaceId:'chat',providerBoundInterfaceId:'provider',turnOpen:false,turnState:'idle',lastMovementSeq:100,updatedAtMs:1000};
const movement=(seq,props={})=>({source:'movement',run_id:'home',interface_id:'chat',from_interface_id:'provider',movement_seq:seq,recorded_at_unix_ms:seq*10,event_kind:'event',event_data:{},terminal:false,error:false,...props});
const resumed=(seq)=>movement(seq,{event_kind:'turn_control',event_data:{action:'resume',status:'acknowledged'}});
const terminal=(seq)=>movement(seq,{event_kind:'message',terminal:true});
test('catalog and account receipts do not turn idle into working',()=>{
 for(const event_kind of ['event','rate_limits']) {
  const next=reconcile(idle,[movement(101,{event_kind})]);
  assert.equal(next.turnOpen,false);assert.equal(next.turnState,'idle');assert.equal(next.lastMovementSeq,101);
 }
});
test('older history, duplicate receipts, streams and other runs cannot overwrite projected state',()=>{
 const current={...idle,turnOpen:true,turnState:'waiting'};
 assert.equal(reconcile(current,[terminal(99),terminal(100),movement(101,{source:'stream'}),movement(101,{run_id:'other'})]),current);
});
test('new terminal followed by catalog in the same or separate batches stays idle',()=>{
 const current={...idle,turnOpen:true,turnState:'working'};
 assert.equal(reconcile(current,[movement(103),terminal(102)]).turnOpen,false);
 assert.equal(reconcile(reconcile(current,[terminal(102)]),[movement(103)]).turnOpen,false);
});
test('resume reopens only with a new acknowledged receipt; errors and stop settle it',()=>{
 const open=reconcile(idle,[resumed(101)]);assert.equal(open.turnOpen,true);
 assert.equal(reconcile(open,[terminal(102)]).turnState,'idle');
 assert.equal(reconcile(open,[movement(102,{event_kind:'error',terminal:true,error:true})]).turnState,'failed');
 assert.equal(reconcile(open,[movement(102,{event_kind:'turn_control',terminal:true,event_data:{action:'stop',status:'acknowledged'}})]).turnOpen,false);
 const failed=reconcile(idle,[movement(101,{event_kind:'turn_control',error:true,event_data:{action:'resume',status:'failed'}})]);
 assert.equal(failed.turnOpen,false);assert.equal(failed.turnState,'idle');
});
test('activity and retry progress an already open turn but never invent an idle turn',()=>{
 assert.equal(reconcile({...idle,turnOpen:true,turnState:'waiting'},[movement(101,{event_kind:'reasoning_delta'})]).turnState,'working');
 assert.equal(reconcile(idle,[movement(101,{event_kind:'reasoning_delta'})]).turnOpen,false);
 assert.equal(reconcile({...idle,turnOpen:true,turnState:'working'},[movement(101,{event_kind:'error',error:true,will_retry:true})]).turnOpen,true);
});
test('external provider starts survive stream activity and settle on exact recorded turn boundaries',()=>{
 const open=reconcile(idle,[movement(101,{event_kind:'turn_started'})]);assert.equal(open.turnOpen,true);
 assert.equal(reconcile(open,[movement(102,{source:'stream',event_kind:'reasoning_delta'})]),open);
 assert.equal(reconcile(idle,[movement(101,{event_kind:'turn_started',from_interface_id:'other'})]).turnOpen,false);
 for(const [event_kind,event_data,terminal,error,state] of [
  ['turn_completed',{},false,false,'idle'],['turn_completed',{status:'failed'},false,false,'failed'],
  ['error',{},true,true,'failed'],['interrupted_result',{},false,false,'idle'],
  ['turn_control',{action:'interrupt',status:'acknowledged'},false,false,'idle'],
 ]) {const closed=reconcile(open,[movement(102,{event_kind,event_data,terminal,error}),movement(103)]);
  assert.deepEqual([closed.turnOpen,closed.turnState],[false,state]);assert.equal(reconcile(closed,[movement(101,{event_kind:'turn_started'})]),closed);}
});
test('historic resume message cannot override a later idle projection while messages page in',()=>{
 const message={movementSeq:101,createdAtMs:1010,role:'assistant',eventKind:'turn_control',eventData:{action:'resume',status:'acknowledged'},terminal:false};
 assert.equal(resumeOpen([message],100),true);
 assert.equal(resumeOpen([message],102),false);
 const terminal={...message,movementSeq:102,createdAtMs:1,eventKind:'message',eventData:{},terminal:true};
 const stop={...terminal,eventKind:'turn_control',eventData:{action:'stop',status:'acknowledged'}};
 for(const [outcome,available] of [[terminal,false],[stop,true]]) {assert.equal(resumeOpen([outcome,message]),false);assert.equal(moduleExports.operatorTurnResumeAvailable([outcome,message],false),available);}
 const olderStop={...stop,movementSeq:100,createdAtMs:2000};
 assert.equal(resumeOpen([message,olderStop]),true);assert.equal(moduleExports.operatorTurnResumeAvailable([message,olderStop],false),false);
});
test('listener resume cannot leave an independent busy latch when projection sees completion first',()=>{
 const workspace=fs.readFileSync(path.join(__dirname,'../src/components/chat/useChatWorkspace.ts'),'utf8');
 const ast=ts.createSourceFile('workspace.ts',workspace,ts.ScriptTarget.Latest,true);let callback;
 function visit(node){if(ts.isVariableDeclaration(node)&&node.name.getText()==='reconcileRunReceipt')callback=node.initializer.arguments[0].getText();ts.forEachChild(node,visit);}visit(ast);
 const compiled=ts.transpileModule('const callback='+callback+';',{compilerOptions:{target:ts.ScriptTarget.ES2022}}).outputText;
 let rows=[idle],targets={};const projectedRunsRef={current:[idle]},runReceiptSequenceRef={current:new Map()};
 const listener=Function('projectedRunsRef','runReceiptSequenceRef','movementArtifact','setRuns','reconcileChatRunReceipts','setPendingTurnControls','setInFlightTurnControlTargets','isChatReplyTerminal',compiled+'return callback;')(projectedRunsRef,runReceiptSequenceRef,a=>a.source==='movement',update=>rows=update(rows),reconcile,()=>{},update=>targets=update(targets),moduleExports.isChatReplyTerminal);
 listener('home',[resumed(101)]);assert.equal(rows[0].turnOpen,true);assert.deepEqual(targets,{});
 rows=[{...idle,lastMovementSeq:102}];projectedRunsRef.current=rows;
 listener('home',[terminal(102)]);assert.equal(rows[0].turnOpen,false);assert.deepEqual(targets,{});
});
