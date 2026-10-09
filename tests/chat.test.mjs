import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { JSDOM } from 'jsdom';
import vm from 'node:vm';
import ts from 'typescript';

function chat(invoke, editor) {
  const window = new JSDOM(readFileSync(new URL('../index.html', import.meta.url), 'utf8')).window;
  const notices = [];
  const context = vm.createContext({document:window.document, HTMLButtonElement:window.HTMLButtonElement, invoke, setupMcp:()=>({refresh:async()=>{}}), renderMarkdown:body=>body, externalMarkdownUrl:()=>null, openUrl:async()=>{}});
  const commandsSource=readFileSync(new URL('../src/chat-commands.ts',import.meta.url),'utf8').replace(/^import .*;\n/gm,'').replace(/^export /gm,'');
  const compiledCommands=ts.transpileModule(commandsSource,{compilerOptions:{target:ts.ScriptTarget.ES2020,module:ts.ModuleKind.None}}).outputText;
  vm.runInContext(`globalThis.setupChatCommands = (() => {${compiledCommands}; globalThis.connectionId = connectionId; return setupChatCommands;})();`,context);
  const source = readFileSync(new URL('../src/chat.ts', import.meta.url), 'utf8').replace(/^import .*;\n/gm,'').replace(/^export /gm,'');
  vm.runInContext(ts.transpileModule(source, {compilerOptions:{target:ts.ScriptTarget.ES2020,module:ts.ModuleKind.None}}).outputText,context);
  const controller = context.setupChat(()=>{},message=>notices.push(message),editor);
  return {controller, notices, element:id=>window.document.getElementById(id), submit(id) {window.document.getElementById(id).dispatchEvent(new window.Event('submit',{cancelable:true}));}, settle:()=>new Promise(resolve=>setImmediate(resolve))};
}
const notebook = {purpose:'Explore ideas',messages:[],pages:2};
const connection = {provider:'openai',model:'gpt-4.1-mini',configured:true};

test('chat sends selected notebook and saves edited reply using its own page command', async()=>{
  const calls=[];
  const app=chat(async(command,args)=>{
    calls.push([command,args]);
    if(command==='get_notebook_chat')return notebook;
    if(command==='get_ai_connections')return [connection];
    if(command==='chat_notebook')return {messages:[{role:'user',content:'draft'},{role:'assistant',content:'# Draft'}],included_pages:1,total_pages:2};
  });
  assert.equal(await app.controller.open('Ideas'),true);
  app.element('chat-input').value='draft';app.submit('chat-form');await app.settle();
  assert.equal(calls.find(([c])=>c==='chat_notebook')[1].notebook,'Ideas');
  assert.equal(app.element('chat-input').value,'');
  assert.match(app.element('chat-context').textContent,/1 of 2/);
  app.element('chat-log').querySelector('button').click();
  assert.equal(app.element('chat-page').value,'# Draft');
  app.element('chat-page').value='# Edited page';app.submit('chat-page-form');await app.settle();
  const save=calls.find(([c])=>c==='save_chat_page');
  assert.equal(save[1].notebook,'Ideas');assert.equal(save[1].content,'# Edited page');
  assert.equal(calls.some(([c])=>c==='commit_entry'),false,'saving chat must not clear the editor draft');
});

test('failed requests retain message for retry and block notebook switching while pending',async()=>{
  let reject;
  const pending=new Promise((_,r)=>{reject=r;});
  const app=chat(async command=>command==='get_notebook_chat'?notebook:command==='get_ai_connections'?[connection]:pending);
  await app.controller.open('Ideas');
  app.element('chat-input').value='keep this';app.submit('chat-form');
  assert.equal(await app.controller.open('Work'),false);
  assert.equal(app.element('chat-send').disabled,true);
  reject(new Error('offline'));await app.settle();
  assert.equal(app.element('chat-input').value,'keep this');
  assert.equal(app.element('chat-send').disabled,false);
  assert.ok(app.notices.some(n=>n.includes('offline')));
});

test('missing provider key opens connection settings without sending notebook data',async()=>{
  const calls=[];
  const app=chat(async command=>{calls.push(command);return command==='get_notebook_chat'?notebook:[];});
  await app.controller.open('Ideas');app.element('chat-input').value='draft';app.submit('chat-form');await app.settle();
  assert.equal(app.element('ai-settings').open,true);
  assert.equal(calls.includes('chat_notebook'),false);
});

test('saved histories reopen and successful connection save clears key input',async()=>{
  const app=chat(async command=>command==='get_notebook_chat'?{...notebook,messages:[{role:'assistant',content:'saved answer'}]}:[connection]);
  await app.controller.open('Ideas');assert.match(app.element('chat-log').textContent,/saved answer/);
  app.element('ai-key').value='private';app.submit('ai-form');await app.settle();
  assert.equal(app.element('ai-key').value,'');
});

test('tool proposals show original contents and only apply after explicit button click',async()=>{
  const calls=[];
  const proposal={id:'proposal1',name:'delete_entry',arguments:{filename:'a.md'},before:[['a.md','Original page']],applied:false};
  const app=chat(async(command,args)=>{
    calls.push([command,args]);
    if(command==='get_notebook_chat')return {...notebook,messages:[{role:'assistant',content:'Review this deletion.',proposals:[proposal]}]};
    if(command==='get_ai_connections')return [connection];
    if(command==='apply_chat_proposal')return {deleted:'a.md'};
  });
  await app.controller.open('Ideas');
  assert.equal(calls.some(([c])=>c==='apply_chat_proposal'),false);
  assert.match(app.element('chat-log').textContent,/Original page/);
  const button=app.element('chat-log').querySelector('.tool-proposal button');button.click();await app.settle();
  const applied=calls.find(([c])=>c==='apply_chat_proposal');assert.equal(applied[1].id,'proposal1');assert.equal(applied[1].notebook,'Ideas');
  assert.equal(app.element('chat-log').querySelector('.tool-proposal button').textContent,'Applied');
});

test('external proposals require approval and display returned MCP results',async()=>{
  const calls=[];
  const proposal={id:'mcp1',name:'mcp_call',arguments:{server:'research',endpoint:'https://example.com/mcp',tool:'search',input:{query:'ideas'},schema:{type:'object'}},before:[],applied:false};
  const app=chat(async(command,args)=>{
    calls.push([command,args]);
    if(command==='get_notebook_chat')return {...notebook,messages:[{role:'assistant',content:'Review the external search.',proposals:[proposal]}]};
    if(command==='get_ai_connections')return [connection];
    if(command==='apply_chat_proposal')return {content:[{type:'text',text:'Found a document'}]};
  });
  await app.controller.open('Ideas');
  const button=app.element('chat-log').querySelector('.tool-proposal button');
  assert.equal(button.textContent,'Approve & run external tool');assert.equal(calls.some(([c])=>c==='apply_chat_proposal'),false);
  button.click();await app.settle();
  assert.match(app.element('chat-log').textContent,/Found a document/);
  assert.equal(app.element('chat-log').querySelector('.tool-proposal button').textContent,'Attempted');
});


test('Azure connection form saves endpoint and deployment and clears its key',async()=>{
  const calls=[];
  const azure={provider:'azure_openai',model:'my-deployment',endpoint:'https://resource.openai.azure.com',configured:true};
  const app=chat(async(command,args)=>{calls.push([command,args]);return command==='get_notebook_chat'?notebook:command==='get_ai_connections'?[azure]:null;});
  await app.controller.open('Ideas');
  app.element('ai-connection').value='azure_openai';
  app.element('ai-connection').dispatchEvent(new (app.element('ai-provider').ownerDocument.defaultView.Event)('change'));
  assert.equal(app.element('ai-endpoint-field').hidden,false);
  assert.equal(app.element('ai-model').value,'my-deployment');
  assert.equal(app.element('ai-endpoint').value,azure.endpoint);
  app.element('ai-key').value='secret';app.submit('ai-form');await app.settle();
  const saved=calls.find(([c])=>c==='set_ai_connection')[1].connection;
  assert.equal(saved.provider,'azure_openai');assert.equal(saved.endpoint,azure.endpoint);assert.equal(saved.model,'my-deployment');
  assert.equal(app.element('ai-key').value,'');
});


test('editor snapshot goes with chat and replacement only applies after review', async()=>{
  const calls=[];let content='draft';let locked=false;
  const context={content:'draft',notebook:'Ideas',revision:1};
  const proposal={id:'editor1',name:'replace_editor',arguments:{editor:context,content:'# Draft'},before:[['Unsaved editor draft','draft']],applied:false};
  const app=chat(async(command,args)=>{
    calls.push([command,args]);
    if(command==='get_notebook_chat')return notebook;
    if(command==='get_ai_connections')return [connection];
    if(command==='chat_notebook')return {messages:[{role:'assistant',content:'Format draft',proposals:[proposal]}],included_pages:0,total_pages:2};
    if(command==='apply_chat_proposal'){assert.equal(locked,true);return {editor_content:'# Draft',action:'replace'};}
  },{snapshot:()=>context,lock:()=>{locked=true;return true;},unlock:()=>{locked=false;},replace:async next=>{content=next;}});
  await app.controller.open('Ideas');app.element('chat-input').value='Format my draft';app.submit('chat-form');await app.settle();
  assert.equal(calls.find(([c])=>c==='chat_notebook')[1].editor.content,'draft');
  assert.equal(content,'draft');
  const apply=[...app.element('chat-log').querySelectorAll('button')].find(b=>b.textContent==='Apply change');
  apply.click();await app.settle();
  assert.equal(content,'# Draft');assert.equal(locked,false);
});

test('slash provider and model commands stay local and preserve the editor draft',async()=>{
  const calls=[];
  const connections=[{...connection},{provider:'azure_openai',model:'old-deployment',endpoint:'https://resource.openai.azure.com',configured:true}];
  const app=chat(async(command,args)=>{
    calls.push([command,args]);
    if(command==='get_notebook_chat')return notebook;
    if(command==='get_ai_connections')return connections;
    if(command==='set_ai_model'){connections.find(c=>c.provider===args.provider).model=args.model;return;}
    if(command==='list_ai_models')return {models:['old-deployment','new-deployment'],note:'Available deployments'};
  },{snapshot:()=>{throw new Error('Local commands must not read editor contents');}});
  await app.controller.open('Ideas');
  const send=async text=>{app.element('chat-input').value=text;app.submit('chat-form');await app.settle();};
  await send('/provider list');assert.match(app.element('chat-command-results').textContent,/Azure OpenAI/);
  await send('/provider azure_openai');assert.equal(app.element('chat-provider').value,'azure_openai');
  await send('/model list');assert.match(app.element('chat-command-results').textContent,/new-deployment/);
  await send('/model new-deployment');assert.equal(connections[1].model,'new-deployment');
  assert.equal(connections[1].endpoint,'https://resource.openai.azure.com');
  assert.equal(calls.some(([c])=>c==='set_ai_connection'),false);
  assert.equal(calls.some(([c])=>c==='chat_notebook'),false);
  assert.equal(app.element('chat-model-current').textContent,'new-deployment');
  assert.equal(app.element('chat-input').value,'');
});

test('MCP commands show servers and reconnect using saved backend credentials',async()=>{
  const calls=[];const servers=[{id:'research',url:'https://example.com/mcp',enabled:false,tools:[]}];
  const app=chat(async(command,args)=>{
    calls.push([command,args]);
    if(command==='get_notebook_chat')return notebook;
    if(command==='get_ai_connections')return [connection];
    if(command==='list_mcp_servers')return servers;
    if(command==='reconnect_mcp_server')return {...servers[0],tools:[{name:'search'}]};
    if(command==='enable_mcp_server')servers[0].enabled=args.enabled;
  });
  await app.controller.open('Ideas');
  app.element('chat-input').value='/mcp';app.submit('chat-form');await app.settle();
  assert.match(app.element('chat-command-results').textContent,/research/);assert.match(app.element('chat-command-results').textContent,/Disabled/);
  app.element('chat-input').value='/mcp reconnect research';app.submit('chat-form');await app.settle();
  assert.equal(calls.find(([c])=>c==='reconnect_mcp_server')[1].id,'research');assert.equal(servers[0].enabled,false);
  app.element('chat-input').value='/mcp enable research';app.submit('chat-form');await app.settle();assert.equal(servers[0].enabled,true);
  assert.equal(calls.some(([c])=>c==='connect_mcp_server'||c==='chat_notebook'),false);
});

test('new and resume commands keep chat switching local and preserve proposals',async()=>{
  const calls=[];
  const sessions=[{id:'older',title:'Old conversation',updated_at:'2026-10-09T00:00:00Z',messages:2,active:false}];
  const app=chat(async(command,args)=>{
    calls.push([command,args]);
    if(command==='get_notebook_chat')return notebook;
    if(command==='get_ai_connections')return [connection];
    if(command==='list_notebook_chats')return sessions;
    if(command==='resume_notebook_chat')return {...notebook,messages:[{role:'assistant',content:'Resumed answer',proposals:[{id:'already',name:'delete_entry',arguments:{filename:'a.md'},before:[],applied:true}]}]};
  });
  await app.controller.open('Ideas');
  app.element('chat-input').value='/new';app.submit('chat-form');await app.settle();
  assert.equal(calls.find(([c])=>c==='clear_notebook_chat')[1].notebook,'Ideas');
  app.element('chat-input').value='/resume';app.submit('chat-form');await app.settle();
  assert.match(app.element('chat-command-results').textContent,/Old conversation/);
  app.element('chat-command-results').querySelector('button').click();await app.settle();
  assert.equal(calls.find(([c])=>c==='resume_notebook_chat')[1].id,'older');
  assert.match(app.element('chat-log').textContent,/Resumed answer/);
  assert.equal(app.element('chat-log').querySelector('.tool-proposal button').disabled,true);
  assert.equal(calls.some(([c])=>c==='chat_notebook'||c==='apply_chat_proposal'),false);
});

test('unknown commands retain input and unavailable providers cannot be selected',async()=>{
  const calls=[];
  const app=chat(async(command,args)=>{calls.push([command,args]);return command==='get_notebook_chat'?notebook:command==='get_ai_connections'?[connection]:null;});
  await app.controller.open('Ideas');
  app.element('chat-input').value='/provider claude';app.submit('chat-form');await app.settle();
  assert.equal(app.element('chat-provider').value,'openai');assert.match(app.element('chat-command-results').textContent,/Connect Claude/);
  app.element('chat-input').value='/missing';app.submit('chat-form');await app.settle();
  assert.equal(app.element('chat-input').value,'/missing');assert.match(app.element('chat-command-results').textContent,/Unknown command/);
  assert.equal(calls.some(([c])=>c==='chat_notebook'),false);
});

test('slash suggestions accept keyboard navigation and failed commands keep the composer usable',async()=>{
  let reject;const pending=new Promise((_,r)=>{reject=r;});
  const app=chat(async command=>command==='get_notebook_chat'?notebook:command==='get_ai_connections'?[connection]:command==='list_ai_models'?pending:null);
  await app.controller.open('Ideas');
  const input=app.element('chat-input');const win=input.ownerDocument.defaultView;
  input.value='/';input.dispatchEvent(new win.Event('input'));
  assert.equal(app.element('chat-command-suggestions').hidden,false);
  input.dispatchEvent(new win.KeyboardEvent('keydown',{key:'ArrowDown',cancelable:true}));
  input.dispatchEvent(new win.KeyboardEvent('keydown',{key:'Tab',cancelable:true}));
  assert.equal(input.value,'/model ');
  app.submit('chat-form');await app.settle();assert.equal(app.element('chat-send').disabled,true);
  assert.equal(await app.controller.open('Other'),false);
  reject(new Error('Model list offline'));await app.settle();
  assert.equal(app.element('chat-send').disabled,false);assert.match(app.element('chat-command-results').textContent,/Model list offline/);
});

test('named Azure connections keep separate saved models and route chat by connection name',async()=>{
  const calls=[];
  const connections=[{id:'azure_openai',provider:'azure_openai',model:'main-one',models:['main-one','main-two'],configured:true,endpoint:'https://main.openai.azure.com'},
    {id:'azure-voice',provider:'azure_openai',model:'voice-one',models:['voice-one','voice-two'],configured:true,endpoint:'https://voice.openai.azure.com'}];
  const app=chat(async(command,args)=>{
    calls.push([command,args]);
    if(command==='get_notebook_chat')return notebook;
    if(command==='get_ai_connections')return connections;
    if(command==='set_ai_model'){const connection=connections.find(c=>c.id===args.provider);connection.model=args.model;if(!connection.models.includes(args.model))connection.models.push(args.model);}
    if(command==='save_ai_model')connections.find(c=>c.id===args.provider).models.push(args.model);
    if(command==='chat_notebook')return {messages:[{role:'user',content:args.message},{role:'assistant',content:'Voice connection reply'}],included_pages:0,total_pages:2};
  });
  await app.controller.open('Ideas');
  const send=async text=>{app.element('chat-input').value=text;app.submit('chat-form');await app.settle();};
  await send('/provider azure-voice');assert.equal(app.element('chat-provider').value,'azure-voice');
  await send('/model saved');assert.match(app.element('chat-command-results').textContent,/voice-two/);
  assert.equal(calls.some(([c])=>c==='list_ai_models'),false,'saved models require no network discovery');
  await send('/model add voice-three');assert.equal(connections[1].model,'voice-one');assert.ok(connections[1].models.includes('voice-three'));
  await send('/model voice-two');assert.equal(connections[1].model,'voice-two');assert.equal(connections[0].model,'main-one');
  await send('Hello');assert.equal(calls.find(([c])=>c==='chat_notebook')[1].provider,'azure-voice');
  assert.match(app.element('chat-log').textContent,/Voice connection reply/);
  assert.equal(connections[1].endpoint,'https://voice.openai.azure.com');
});

test('connection settings create a named profile with a saved model list',async()=>{
  const calls=[];const connections=[{...connection,id:'openai',models:['gpt-4.1-mini']}];
  const app=chat(async(command,args)=>{
    calls.push([command,args]);
    if(command==='get_notebook_chat')return notebook;
    if(command==='get_ai_connections')return connections;
    if(command==='set_ai_connection')connections.push({...args.connection,id:args.id,configured:true});
  });
  await app.controller.open('Ideas');
  app.element('ai-provider').value='azure_openai';app.element('ai-connection-name').value='azure-voice';
  app.element('ai-model').value='voice-one';app.element('ai-saved-models').value='voice-one\nvoice-two';
  app.element('ai-endpoint').value='https://voice.openai.azure.com';app.element('ai-key').value='private-key';
  app.submit('ai-form');await app.settle();
  const saved=calls.find(([c])=>c==='set_ai_connection')[1];assert.equal(saved.id,'azure-voice');assert.equal(saved.connection.provider,'azure_openai');
  assert.deepEqual(Array.from(saved.connection.models),['voice-one','voice-two']);assert.equal(app.element('ai-key').value,'');
  assert.equal(app.element('chat-provider').value,'azure-voice');assert.equal(connections[0].model,'gpt-4.1-mini');
});
