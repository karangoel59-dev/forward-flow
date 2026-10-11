import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { JSDOM } from 'jsdom';
import vm from 'node:vm';
import ts from 'typescript';

function conflicts(invoke) {
  const window = new JSDOM(readFileSync(new URL('../index.html', import.meta.url), 'utf8')).window;
  const notices = [];
  const view = window.document.getElementById('conflict');
  const context = vm.createContext({window, document: window.document, invoke});
  const source = readFileSync(new URL('../src/conflicts.ts', import.meta.url), 'utf8').replace(/^import .*;\n/gm, '').replace(/^export /gm, '');
  vm.runInContext(ts.transpileModule(source, {compilerOptions: {target: ts.ScriptTarget.ES2020, module: ts.ModuleKind.None}}).outputText, context);
  const controller = context.setupConflicts(text => notices.push(text), () => { view.hidden = false; }, () => { view.hidden = true; });
  return {controller, notices, view, element: id => window.document.getElementById(id), all: selector => [...window.document.querySelectorAll(selector)], window, settle: () => new Promise(resolve => setImmediate(resolve))};
}
const found = {local: 'aaa', remote: 'bbb', files: [
  {path: 'Ideas/a.md', mine: 'tags: [mine]\n<b>prose</b>', theirs: 'tags: [theirs]\n<b>prose</b>', both: 'combine'},
  {path: 'notes.txt', mine: null, theirs: 'kept elsewhere', both: null},
]};
const pick = (app, name, value) => {
  const radio = app.all(`input[name="${name}"]`).find(input => input.value === value);
  radio.checked = true; radio.dispatchEvent(new app.window.Event('change'));
};

test('sync conflicts show both versions as text and resolve only after every file is chosen', async () => {
  const calls = [];
  const app = conflicts(async (command, args) => { calls.push([command, args]); if (command === 'get_conflicts') return found; });
  app.controller.flag(2);
  assert.equal(app.element('conflict-banner').hidden, false);
  assert.match(app.element('conflict-banner').textContent, /2 pages changed on two devices/);

  app.element('conflict-banner').click(); await app.settle();
  assert.equal(app.view.hidden, false);
  const versions = app.all('.conflict-version pre').map(pre => pre.textContent);
  assert.deepEqual(versions, ['tags: [mine]\n<b>prose</b>', 'tags: [theirs]\n<b>prose</b>', 'Deleted on this device', 'kept elsewhere']);
  assert.equal(app.all('#conflict-list b').length, 0, 'page contents are never rendered as HTML');
  assert.deepEqual(app.all('input[name="conflict-0"]').map(i => i.value), ['mine', 'theirs', 'both']);
  assert.deepEqual(app.all('input[name="conflict-1"]').map(i => i.value), ['mine', 'theirs'], 'a deleted side cannot be kept as both');

  assert.equal(app.element('conflict-apply').disabled, true);
  pick(app, 'conflict-0', 'both');
  assert.equal(app.element('conflict-apply').disabled, true);
  app.element('conflict-apply').click(); await app.settle();
  assert.equal(calls.some(([c]) => c === 'resolve_conflicts'), false);
  pick(app, 'conflict-1', 'theirs');
  assert.equal(app.element('conflict-apply').disabled, false);

  app.element('conflict-apply').click(); await app.settle();
  assert.equal(JSON.stringify(calls.find(([c]) => c === 'resolve_conflicts')[1]), JSON.stringify({local: 'aaa', remote: 'bbb', choices: [{path: 'Ideas/a.md', keep: 'both'}, {path: 'notes.txt', keep: 'theirs'}]}));
  assert.equal(app.view.hidden, true);
  assert.equal(app.element('conflict-banner').hidden, true);
  assert.match(app.notices.at(-1), /resolved/);
});

test('a failed resolution keeps the choices and the review open', async () => {
  const app = conflicts(async command => { if (command === 'get_conflicts') return {...found, files: found.files.slice(0, 1)}; throw new Error('The vault changed since you reviewed this conflict. Review it again.'); });
  app.controller.flag(1);
  await app.controller.review();
  pick(app, 'conflict-0', 'mine');
  app.element('conflict-apply').click(); await app.settle();
  assert.equal(app.view.hidden, false);
  assert.match(app.element('conflict-status').textContent, /Review it again/);
  assert.equal(app.all('input[name="conflict-0"]').find(i => i.value === 'mine').checked, true);
  assert.equal(app.element('conflict-apply').disabled, false);
  assert.equal(app.element('conflict-banner').hidden, false);
});

test('a review with nothing left to resolve closes itself and resumes sync', async () => {
  const calls = [];
  const app = conflicts(async command => { calls.push(command); if (command === 'get_conflicts') return {local: '', remote: '', files: []}; });
  app.controller.flag(1);
  assert.equal(await app.controller.review(), false);
  assert.equal(app.view.hidden, true);
  assert.equal(app.element('conflict-banner').hidden, true);
  assert.ok(calls.includes('resync_vault'));
});
