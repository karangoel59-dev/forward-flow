import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import vm from 'node:vm';
import ts from 'typescript';

function frontend(invoke, chatOpen = async () => true) {
  const elements = new Map();
  const timers = new Map();
  let nextTimer = 0;
  class Element {
    value = '';
    readOnly = false;
    style = {};
    children = [];
    replaceChildren() { this.children = []; }
    append(...items) { this.children.push(...items); }
    scrollIntoView() {}
    querySelector() { return this; }
    classList = { add() {}, remove() {} };
    addEventListener() {}
    setAttribute() {}
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
    createElement() { return new Element(); },
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
    HTMLSelectElement: class extends Element {},
    getCurrentWindow: () => ({}),
    listen: () => Promise.resolve(),
    invoke,
    setupSettings: () => {},
    setupChat: () => ({open: chatOpen}),
    renderMarkdown: body => body,
    externalMarkdownUrl: () => null,
    openUrl: async () => {},
  });
  const source = readFileSync(new URL('../src/main.ts', import.meta.url), 'utf8')
    .replace(/^import .*;\n/gm, '')
    .replace(/\nboot\(\);\s*$/, '');
  vm.runInContext(ts.transpileModule(source, {
    compilerOptions: { target: ts.ScriptTarget.ES2020, module: ts.ModuleKind.None },
  }).outputText, context);
  return {
    run: (code) => vm.runInContext(code, context),
    element: id => document.getElementById(id),
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


test('committing saves into the selected notebook', async () => {
  let saved;
  const app = frontend(async (command, args) => {
    if (command === 'commit_entry') { saved = args; return { words: 2 }; }
  });
  app.editor.value = 'travel notes';
  app.element('write-notebook').value = 'Travel';
  await app.run('commit()');
  assert.equal(saved.notebook, 'Travel');
  assert.equal(saved.content, 'travel notes');
});

test('archive notebook filters combine with search, linking spans all notebooks', () => {
  const app = frontend(async () => {});
  app.run(`entries = [
    {path: 'a.md', name: 'a', notebook: '', created: '', preview: 'river', words: 1, tags: [], links: []},
    {path: 'Travel/b.md', name: 'b', notebook: 'Travel', created: '', preview: 'river trip', words: 2, tags: ['trip'], links: []},
    {path: 'Work/c.md', name: 'c', notebook: 'Work', created: '', preview: 'office', words: 1, tags: [], links: []}
  ]`);
  app.element('picker-notebook').value = 'Travel';
  app.run('renderPicker()');
  assert.equal(app.run('shown.map(e => e.name).join()'), 'b');
  app.element('picker-filter').value = '#trip';
  app.run('renderPicker()');
  assert.equal(app.run('shown.length'), 1);
  app.element('picker-filter').value = '';
  app.run('linkFor = "Travel/b.md"; renderPicker()');
  assert.equal(app.run('shown.map(e => e.name).join()'), 'c,a');
  assert.equal(app.element('picker-notebook').disabled, true);
  app.run('linkFor = null');
  app.element('picker-notebook').value = '';
  app.run('renderPicker()');
  assert.equal(app.run('shown.map(e => e.name).join()'), 'a');
});

test('creating a notebook selects it for the next entry', async () => {
  const app = frontend(async command => command === 'create_notebook' ? 'Ideas' : ['Ideas']);
  app.run('beginNotebook()');
  app.element('notebook-name').value = 'Ideas';
  await app.run('createNotebook()');
  assert.equal(app.element('write-notebook').value, 'Ideas');
  assert.equal(app.run('mode'), 'write');
});

test('notebook refresh retains selections and falls back when a folder disappears', async () => {
  let notebooks = ['Ideas', 'Work'];
  const app = frontend(async () => notebooks);
  app.element('write-notebook').value = 'Ideas';
  app.element('picker-notebook').value = '*';
  await app.run('loadNotebooks()');
  assert.equal(app.element('write-notebook').value, 'Ideas');
  notebooks = ['Work'];
  await app.run('loadNotebooks()');
  assert.equal(app.element('write-notebook').value, '');
  assert.equal(app.element('picker-notebook').value, '*');
});

test('moving an entry opens its new path and refreshes cached metadata', async () => {
  let moved;
  const meta = {path: 'Ideas/a.md', name: 'a', notebook: 'Ideas', created: '', words: 1, tags: [], links: []};
  const app = frontend(async (command, args) => {
    if (command === 'move_entry') { moved = args; return meta; }
    if (command === 'read_entry') return { meta, body: 'hello', related: [] };
  });
  app.run('current = {meta: {path: "a.md", notebook: ""}}; entries = [current.meta]');
  app.element('reader-notebook').value = 'Ideas';
  await app.run('moveCurrentEntry()');
  assert.equal(moved.path, 'a.md');
  assert.equal(moved.notebook, 'Ideas');
  assert.equal(app.run('current.meta.path'), 'Ideas/a.md');
  assert.equal(app.run('entries[0].notebook'), 'Ideas');
});

test('failed move keeps the current entry and restores its notebook selection', async () => {
  const app = frontend(async () => { throw new Error('collision'); });
  app.run('current = {meta: {path: "a.md", notebook: ""}}');
  app.element('reader-notebook').value = 'Ideas';
  await app.run('moveCurrentEntry()');
  assert.equal(app.run('current.meta.path'), 'a.md');
  assert.equal(app.element('reader-notebook').value, '');
  assert.equal(app.element('reader-move').disabled, false);
});


test('opening notebook chat keeps editor visible and retains its draft', async () => {
  const opened = [];
  const app = frontend(async command => command === 'list_notebooks' ? ['Ideas'] : null, async notebook => { opened.push(notebook); return true; });
  app.editor.value = 'Keep writing here';
  await app.run('openNotebookChat("Ideas")');
  assert.deepEqual(opened, ['Ideas']);
  assert.equal(app.run('mode'), 'write');
  assert.equal(app.element('write').style.visibility, 'visible');
  assert.equal(app.element('chat').hidden, false);
  assert.equal(app.element('write-notebook').value, 'Ideas');
  assert.equal(app.editor.value, 'Keep writing here');
});

test('rejected chat notebook switch restores editor notebook selection', async () => {
  let allow = true;
  const app = frontend(async () => null, async () => allow);
  await app.run('openNotebookChat("Ideas")');
  allow = false;
  app.element('write-notebook').value = 'Work';
  assert.equal(await app.run('syncWritingChat()'), false);
  assert.equal(app.element('write-notebook').value, 'Ideas');
  assert.equal(app.run('chatNotebook'), 'Ideas');
});
