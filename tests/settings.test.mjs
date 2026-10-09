import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {test} from 'node:test';
import {JSDOM} from 'jsdom';
import vm from 'node:vm';
import ts from 'typescript';
const tick = () => new Promise(resolve => setImmediate(resolve));
function setup() {
  const window = new JSDOM(readFileSync(new URL('../index.html', import.meta.url), 'utf8')).window;
  const calls = []; const notices = []; let refreshed = 0;
  const context = vm.createContext({document:window.document, invoke:async (cmd,args) => {calls.push([cmd,args]);return 'Imported';},save:async()=>'/tmp/config.json'});
  const source = readFileSync(new URL('../src/settings.ts',import.meta.url),'utf8').replace(/^import .*;\n/gm,'').replace(/^export /gm,'');
  vm.runInContext(ts.transpileModule(source,{compilerOptions:{target:ts.ScriptTarget.ES2020,module:ts.ModuleKind.None}}).outputText,context);
  context.setupSettings(async()=>{refreshed++;},text=>notices.push(text));
  const el = id=>window.document.getElementById(id);
  async function upload(config) {
    Object.defineProperty(el('config-file'),'files',{configurable:true,value:[{size:100,text:async()=>JSON.stringify(config)}]});
    el('config-file').dispatchEvent(new window.Event('change')); await tick();
  }
  return {el,upload,calls,notices,get refreshed(){return refreshed;}};
}
test('config upload is reviewed before applying and never displays credentials',async()=>{
  const app = setup();
  await app.upload({version:1,git:{remote:'https://user:secret@github.com/a/b.git'},models:{openai:{api_key:'secret'}},mcp_servers:{research:{token:'secret'}}});
  assert.equal(app.calls.length,0); assert.equal(app.el('config-review').hidden,false);
  assert.match(app.el('config-status').textContent,/1 model connections, 1 MCP servers/);
  assert.ok(!app.el('config-status').textContent.includes('secret'));
  app.el('config-apply').click(); await tick();
  assert.equal(app.calls[0][0],'import_settings'); assert.match(app.calls[0][1].text,/secret/);
  assert.equal(app.el('config-review').hidden,true); assert.equal(app.refreshed,1);
});
test('cancel and unsupported versions cannot import, export excludes secrets by default',async()=>{
  const app = setup(); await app.upload({version:1});
  app.el('config-cancel').click(); app.el('config-apply').click(); await tick(); assert.equal(app.calls.length,0);
  await app.upload({version:2}); assert.equal(app.el('config-review').hidden,true); assert.match(app.notices[0],/Unsupported/);
  app.el('settings-export-config').click(); await tick();
  assert.equal(app.calls[0][1].includeSecrets,false);
  app.el('config-secrets').checked=true; app.el('settings-export-config').click(); await tick();
  assert.equal(app.calls[1][1].includeSecrets,true);
});
