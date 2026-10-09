import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { JSDOM } from 'jsdom';
import vm from 'node:vm';
import ts from 'typescript';

function chat(invoke) {
  const window = new JSDOM(readFileSync(new URL('../index.html', import.meta.url), 'utf8')).window;
  const notices = [];
  const context = vm.createContext({document:window.document, invoke, setupMcp:()=>({refresh:async()=>{}}), renderMarkdown:body=>body, externalMarkdownUrl:()=>null, openUrl:async()=>{}});
  const source = readFileSync(new URL('../src/chat.ts', import.meta.url), 'utf8').replace(/^import .*;\n/gm,'').replace(/^export /gm,'');
  vm.runInContext(ts.transpileModule(source, {compilerOptions:{target:ts.ScriptTarget.ES2020,module:ts.ModuleKind.None}}).outputText,context);
  const controller = context.setupChat(()=>{},message=>notices.push(message));
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
