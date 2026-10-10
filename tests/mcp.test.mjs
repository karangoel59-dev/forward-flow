import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { JSDOM } from 'jsdom';
import vm from 'node:vm';
import ts from 'typescript';
test('MCP settings connect, list discovered tools, clear tokens, and toggle servers',async()=>{
  const window=new JSDOM(readFileSync(new URL('../index.html',import.meta.url),'utf8')).window;
  const calls=[];let servers=[];
  const context=vm.createContext({document:window.document,invoke:async(command,args)=>{
    calls.push([command,args]);
    if(command==='list_mcp_servers')return servers;
    if(command==='connect_mcp_server'){servers=[{...args.server,authenticated:true,tool_discovery:true,tools:[{name:'search',description:'Find documents'}]}];return servers[0];}
    if(command==='enable_mcp_server')servers[0].enabled=args.enabled;
    if(command==='remove_mcp_server')servers=[];
  }});
  const source=readFileSync(new URL('../src/mcp.ts',import.meta.url),'utf8').replace(/^import .*;\n/gm,'').replace(/^export /gm,'');
  vm.runInContext(ts.transpileModule(source,{compilerOptions:{target:ts.ScriptTarget.ES2020,module:ts.ModuleKind.None}}).outputText,context);
  const controller=context.setupMcp(()=>{});await controller.refresh();
  const el=id=>window.document.getElementById(id);
  el('mcp-name').value='research';el('mcp-url').value='https://example.com/mcp';el('mcp-token').value='private';
  el('mcp-form').dispatchEvent(new window.Event('submit',{cancelable:true}));await new Promise(resolve=>setImmediate(resolve));
  assert.equal(calls.find(([c])=>c==='connect_mcp_server')[1].server.token,'private');assert.equal(el('mcp-token').value,'');
  assert.match(el('mcp-list').textContent,/search/);
  assert.match(el('mcp-list').textContent,/All tools available to chat through tool discovery/);
  [...el('mcp-list').querySelectorAll('button')].find(b=>b.textContent==='Disable').click();await new Promise(resolve=>setImmediate(resolve));
  assert.equal(servers[0].enabled,false);
  assert.match(el('mcp-list').textContent,/Disabled for chat/);
  [...el('mcp-list').querySelectorAll('button')].find(b=>b.textContent==='Disconnect').click();await new Promise(resolve=>setImmediate(resolve));
  assert.equal(servers.length,0);
});
