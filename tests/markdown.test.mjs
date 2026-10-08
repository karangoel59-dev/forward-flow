import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { JSDOM } from 'jsdom';
import { marked } from 'marked';
import createDOMPurify from 'dompurify';
import vm from 'node:vm';
import ts from 'typescript';

const window = new JSDOM('').window;
const context = vm.createContext({ marked, DOMPurify: createDOMPurify(window), URL });
const source = readFileSync(new URL('../src/markdown.ts', import.meta.url), 'utf8')
  .replace(/^import .*;\n/gm, '').replace(/^export /gm, '');
vm.runInContext(ts.transpileModule(source, {
  compilerOptions: { target: ts.ScriptTarget.ES2020, module: ts.ModuleKind.None },
}).outputText, context);
const render = body => context.renderMarkdown(body);
const parse = body => new JSDOM(render(body)).window.document;

test('renders headings, emphasis, lists, quotations, and links', () => {
  const document = parse('# Title\n\n**bold** and *italic*\n\n- first\n- second\n\n> quotation\n\n[site](https://example.com)');
  assert.equal(document.querySelector('h1').textContent, 'Title');
  assert.equal(document.querySelector('strong').textContent, 'bold');
  assert.equal(document.querySelector('em').textContent, 'italic');
  assert.equal(document.querySelectorAll('ul li').length, 2);
  assert.equal(document.querySelector('blockquote').textContent.trim(), 'quotation');
  assert.equal(document.querySelector('a').href, 'https://example.com/');
});

test('renders tables and escaped fenced code with indentation intact', () => {
  const document = parse('| A | B |\n| --- | --- |\n| one | two |\n\n```html\n  <script>hello</script>\n```');
  assert.equal(document.querySelectorAll('th').length, 2);
  assert.equal(document.querySelector('td').textContent, 'one');
  assert.equal(document.querySelector('pre code').textContent, '  <script>hello</script>\n');
  assert.equal(document.querySelector('script'), null);
});

test('sanitizes executable HTML, event handlers, unsafe URLs, and embedded views', () => {
  const document = parse('<script>alert(1)</script><img src="x" onerror="alert(1)"><iframe src="https://evil.example"></iframe><svg onload="alert(1)"></svg>\n\n[bad](javascript:alert%281%29)\n\n<a href="data:text/html,test" onclick="alert(1)">data</a>');
  assert.equal(document.querySelector('script, iframe, svg'), null);
  assert.equal(document.querySelector('[onerror], [onclick], [onload]'), null);
  assert.equal(document.querySelector('a[href]'), null);
});

test('entry HTML cannot inject controls or replace app element IDs', () => {
  const document = parse('<form><input id="editor" name="editor"><button>Save</button></form><p id="reader-body" style="position:fixed">prose</p>');
  assert.equal(document.querySelector('form, input, button, [id], [name], [style]'), null);
  assert.ok(document.body.textContent.includes('prose'));
});

test('only full HTTP, HTTPS, and mailto links may open externally', () => {
  for (const url of ['https://example.com/page', 'http://example.com/', 'mailto:hello@example.com']) {
    assert.equal(context.externalMarkdownUrl(url), url);
  }
  for (const url of ['javascript:alert(1)', 'data:text/html,hi', 'file:///tmp/file', '../a.md', '#title', '//example.com']) {
    assert.equal(context.externalMarkdownUrl(url), null);
  }
});
