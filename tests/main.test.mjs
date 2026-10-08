import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import vm from 'node:vm';
import ts from 'typescript';

function frontend(invoke) {
  const elements = new Map();
  const timers = new Map();
  let nextTimer = 0;
  class Element {
    value = '';
    readOnly = false;
    style = {};
    classList = { add() {}, remove() {} };
    addEventListener() {}
    focus() {}
    blur() {}
  }
  const document = {
    documentElement: new Element(),
    getElementById(id) {
      if (!elements.has(id)) elements.set(id, new Element());
      return elements.get(id);
    },
    addEventListener() {},
  };
  const context = vm.createContext({
    document,
    navigator: { maxTouchPoints: 0 },
    window: {
      matchMedia: () => ({ matches: false }),
      addEventListener() {},
      setTimeout(callback) { timers.set(++nextTimer, callback); return nextTimer; },
    },
    clearTimeout(id) { timers.delete(id); },
    requestAnimationFrame() {},
    HTMLInputElement: Element,
    HTMLTextAreaElement: Element,
    getCurrentWindow: () => ({}),
    listen: () => Promise.resolve(),
    invoke,
  });
  const source = readFileSync(new URL('../src/main.ts', import.meta.url), 'utf8')
    .replace(/^import .*;\n/gm, '')
    .replace(/\nboot\(\);\s*$/, '');
  vm.runInContext(ts.transpileModule(source, {
    compilerOptions: { target: ts.ScriptTarget.ES2020, module: ts.ModuleKind.None },
  }).outputText, context);
  return {
    run: (code) => vm.runInContext(code, context),
    editor: document.getElementById('editor'),
    flushTimers() {
      const callbacks = [...timers.values()];
      timers.clear();
      callbacks.forEach(callback => callback());
    },
    timerCount: () => timers.size,
  };
}

test('editor stays read-only through save and animation, then clears', async () => {
  let resolveSave;
  const save = new Promise(resolve => { resolveSave = resolve; });
  const app = frontend(() => save);
  app.editor.value = 'my entry';
  app.run('scheduleDraftSave()');
  const commit = app.run('commit()');
  assert.equal(app.editor.readOnly, true);
  assert.equal(app.timerCount(), 0, 'pending draft save must be canceled');
  resolveSave({ words: 2 });
  await commit;
  assert.equal(app.editor.readOnly, true);
  app.flushTimers();
  assert.equal(app.editor.value, '');
  assert.equal(app.editor.readOnly, false);
});

test('failed save retains the draft and unlocks the editor', async () => {
  const app = frontend(() => Promise.reject(new Error('disk full')));
  app.editor.value = 'keep this';
  await app.run('commit()');
  assert.equal(app.editor.value, 'keep this');
  assert.equal(app.editor.readOnly, false);
  assert.equal(app.run('committing'), false);
  assert.ok(app.timerCount() > 0, 'draft autosave resumes');
});

test('delete confirmation cannot carry over to another entry', async () => {
  const deletions = [];
  const app = frontend(async (command, args) => {
    if (command === 'delete_entry') deletions.push(args.path);
  });
  await app.run('current = {meta: {path: "a.md"}}; deleteCurrentEntry()');
  await app.run('current = {meta: {path: "b.md"}}; deleteCurrentEntry()');
  assert.deepEqual(deletions, []);
  await app.run('deleteCurrentEntry()');
  assert.deepEqual(deletions, ['b.md']);
});

test('leaving the reader clears delete confirmation', async () => {
  const app = frontend(async () => {});
  await app.run('current = {meta: {path: "a.md"}}; deleteCurrentEntry()');
  app.run('show("write")');
  assert.equal(app.run('deletePendingPath'), null);
});
