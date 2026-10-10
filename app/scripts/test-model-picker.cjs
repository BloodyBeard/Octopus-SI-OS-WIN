const assert=require('node:assert/strict');
const {test}=require('node:test');
const fs=require('node:fs'),path=require('node:path');
const ts=require('typescript');
const app=path.resolve(__dirname,'..');
const read=p=>fs.readFileSync(p,'utf8');
const parse=(file,text)=>ts.createSourceFile(file,text,ts.ScriptTarget.Latest,true,ts.ScriptKind.JS);

test('catalog order remains declared when selection and default flags change',()=>{
 const source=read(path.join(app,'electron/projection.cjs'));
 const tree=parse('projection.cjs',source);
 const names=new Set(['providerModelCatalogRows','arrayValue','objectOrEmpty','stringValue']);
 const functions=tree.statements.filter(n=>ts.isFunctionDeclaration(n)&&names.has(n.name.text)).map(n=>n.getText(tree)).join('\n');
 const rows=Function(functions+';return providerModelCatalogRows;')();
 const catalog=[{model:'zeta',display_name:'Zeta'},{model:'alpha',display_name:'Alpha'}];
 for(const selected of ['alpha','zeta'])for(const defaultModel of ['alpha','zeta']){
  const config={model_catalog:catalog.map(m=>({...m,is_default:m.model===defaultModel}))};
  assert.deepEqual(rows({},config,selected,['zeta','alpha']).map(m=>m.model),['zeta','alpha']);
 }
});

test('model and preserved effort are one validated local selection and one persistence write',()=>{
 const text=read(path.join(app,'src/components/chat/useChatWorkspace.ts'));
 const ast=ts.createSourceFile('workspace.ts',text,ts.ScriptTarget.Latest,true,ts.ScriptKind.TS);let fn;
 const visit=n=>{if(ts.isFunctionDeclaration(n)&&n.name?.text==='chooseProviderModel')fn=n;ts.forEachChild(n,visit);};visit(ast);
 const saved=[],state={};const model={model:'new',supportedReasoningEfforts:['high','ultra']};const provider={id:'provider',models:[model]};
 const scope={activeRunIdRef:{current:null},resolvedProviderInterfaceId:'provider',declaredActiveProvider:provider,selectableProviderInterfaces:[provider],providerModels:p=>p.models,providerReasoningEfforts:()=>model.supportedReasoningEfforts,providerDefaultReasoningEffort:()=> 'high',setProviderInterfaceId:v=>state.provider=v,setProviderModel:v=>state.model=v,setReasoningEffort:v=>state.effort=v,saveChatSettings:v=>saved.push(v),accessProfile:'profile'};
 const code=ts.transpileModule(fn.getText(ast),{compilerOptions:{target:ts.ScriptTarget.ES2022}}).outputText;
 const select=Function(...Object.keys(scope),code+';return chooseProviderModel;')(...Object.values(scope));
 select('provider','new','ultra');assert.deepEqual(state,{provider:'provider',model:'new',effort:'ultra'});
 assert.deepEqual(saved,[{providerInterfaceId:'provider',providerModel:'new',reasoningEffort:'ultra'}]);
 select('provider','new','invalid');select('missing','new','high');assert.equal(saved.length,1);assert.equal(state.effort,'ultra');
 model.supportedReasoningEfforts=[];select('provider','new','');assert.equal(state.effort,'');assert.equal(saved.length,2);
 model.supportedReasoningEfforts=['high','ultra'];select('provider','new','ultra');
 scope.activeRunIdRef.current='run';select('other','new','high');assert.equal(state.effort,'ultra');
});
