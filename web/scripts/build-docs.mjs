// Turns the guides in docs/*.md into the pages under docs/guide/, which the site serves.
//
//   bun run docs            write the pages
//   bun run docs --check    fail if the pages on disk are not what the guides would give
//
// The Markdown files are the source; the pages are committed so the site needs no build step.
// A link to a guide or a heading that does not exist stops the build.

import { Marked } from 'marked';
import { readFileSync, writeFileSync, mkdirSync, readdirSync, existsSync, rmSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const docs = join(dirname(fileURLToPath(import.meta.url)), '..', '..', 'docs');
const out = join(docs, 'guide');
const REPO = 'https://github.com/rewtraw/spool';

// The order of the menu. Every guide in docs/ has to be listed here.
const MENU = [
  ['Start here', [['getting-started', 'Getting started'], ['your-library', 'Your library']]],
  ['Using it', [['locations', 'More than one place'], ['helper', 'A helper on the NAS'], ['players', 'On a television'], ['plex', 'Beside Plex'], ['collections', 'Collections and anime'], ['compact', 'Compact versions'], ['local-files', 'Files it recognises'], ['assistants', 'AI assistants']]],
  ['Reference', [['api', 'The HTTP API'], ['architecture', 'How it is put together'], ['ideas', 'Ideas']]],
];
const ORDER = MENU.flatMap(([, pages]) => pages.map(([name]) => name));

const esc = (s) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
// The same ids GitHub gives headings, so a link works in both places.
const slug = (text) => text.toLowerCase().replace(/[^\p{L}\p{N} _-]/gu, '').trim().replace(/ /g, '-');
const page = (name) => (name === 'README' ? 'index' : name);

function render(name) {
  const source = readFileSync(join(docs, `${name}.md`), 'utf8');
  const headings = [];
  const links = [];
  const marked = new Marked({
    gfm: true,
    walkTokens(t) {
      if (t.type !== 'link' || /^[a-z]+:/i.test(t.href)) return;
      const [file, anchor] = t.href.split('#');
      const m = file.match(/^([\w-]*)\.md$/);
      if (file && !m) throw new Error(`${name}.md links to ${t.href}: link to a guide, or give the full address`);
      const target = m ? m[1] : name;
      links.push([target, anchor]);
      t.href = (m ? (target === 'README' ? './' : `${target}.html`) : '') + (anchor ? `#${anchor}` : '');
    },
    renderer: {
      heading({ tokens, depth, text }) {
        const plain = text.replace(/[`*_]|\[([^\]]*)\]\([^)]*\)/g, '$1');
        const id = slug(plain);
        headings.push({ depth, id, text: plain });
        const inner = this.parser.parseInline(tokens);
        return depth === 1 ? `<h1>${inner}</h1>\n` : `<h${depth} id="${id}"><a href="#${id}">${inner}</a></h${depth}>\n`;
      },
    },
  });
  const body = marked.parse(source).replace(/<table>/g, '<div class="scroll"><table>').replace(/<\/table>/g, '</table></div>');
  const title = headings.find((h) => h.depth === 1)?.text;
  if (!title) throw new Error(`${name}.md has no title`);
  return { name, title, body, headings, links };
}

function menu(current) {
  let html = `<a class="home${current.name === 'README' ? ' on' : ''}" href="./">All guides</a>`;
  for (const [group, pages] of MENU) {
    html += `<p>${group}</p>`;
    for (const [name, label] of pages) {
      html += `<a${name === current.name ? ' class="on" aria-current="page"' : ''} href="${name}.html">${label}</a>`;
      if (name === current.name) {
        html += current.headings.filter((h) => h.depth === 2).map((h) => `<a class="sub" href="#${h.id}">${esc(h.text)}</a>`).join('');
      }
    }
  }
  return html;
}

function around(current, titles) {
  const i = ORDER.indexOf(current.name);
  if (i < 0) return '';
  const link = (name, word, cls) => (name ? `<a class="${cls}" href="${name}.html"><span>${word}</span>${esc(titles[name])}</a>` : '<span></span>');
  return `<nav class="around" aria-label="Other guides">${link(ORDER[i - 1], 'Before', 'prev')}${link(ORDER[i + 1], 'Next', 'next')}</nav>`;
}

const STYLE = `
  :root {
    --bg: #0d0d0f; --surface: #151518; --raised: #1d1d21; --line: #2a2a30;
    --text: #efe9dc; --muted: #9d968a; --faint: #6f6a61;
    --accent: #4d78ff; --accent-soft: rgba(77, 120, 255, 0.16);
    --serif: 'Iowan Old Style', 'Palatino Linotype', Palatino, 'Book Antiqua', Georgia, serif;
    --sans: -apple-system, BlinkMacSystemFont, 'Inter', 'Segoe UI', system-ui, sans-serif;
    --mono: ui-monospace, 'SF Mono', Menlo, monospace;
    color-scheme: dark;
  }
  @media (prefers-color-scheme: light) {
    :root {
      --bg: #f6f1e7; --surface: #fffdf8; --raised: #f0e9db; --line: #dcd3c1;
      --text: #1b1a17; --muted: #635e54; --faint: #8d877b;
      --accent: #1f4fd6; --accent-soft: rgba(31, 79, 214, 0.1);
      color-scheme: light;
    }
  }
  * { box-sizing: border-box; }
  html { scroll-behavior: smooth; scroll-padding-top: 20px; }
  body { margin: 0; background: var(--bg); color: var(--text); font: 16.5px/1.65 var(--sans); -webkit-font-smoothing: antialiased; }
  a { color: inherit; text-decoration: none; }
  .wrap { max-width: 1120px; margin: 0 auto; padding: 0 24px; }

  header { display: flex; align-items: center; justify-content: space-between; padding: 22px 0; border-bottom: 1px solid var(--line); }
  .brand { display: flex; align-items: center; gap: 11px; font: 600 23px/1 var(--serif); }
  .brand .mark { width: 30px; height: 30px; background: var(--accent); -webkit-mask: url(../assets/mark.svg) center / contain no-repeat; mask: url(../assets/mark.svg) center / contain no-repeat; }
  .brand small { font: 500 15px/1 var(--sans); color: var(--muted); margin-left: 4px; }
  header nav { display: flex; gap: 26px; font-size: 15px; color: var(--muted); }
  header nav a:hover { color: var(--text); }

  .layout { display: grid; grid-template-columns: 230px minmax(0, 1fr); gap: 56px; padding-top: 40px; }
  .side { font-size: 14.5px; }
  .side .inner { position: sticky; top: 24px; max-height: calc(100vh - 48px); overflow-y: auto; display: grid; gap: 2px; padding-bottom: 24px; }
  .side summary { display: none; }
  .side p { margin: 18px 0 4px; font-size: 12px; letter-spacing: 0.13em; text-transform: uppercase; color: var(--faint); font-weight: 600; }
  .side a { display: block; padding: 5px 10px; border-radius: 7px; color: var(--muted); }
  .side a:hover { color: var(--text); }
  .side a.on { color: var(--text); background: var(--accent-soft); font-weight: 600; }
  .side a.sub { padding: 3px 10px 3px 22px; font-size: 13.5px; }

  article { max-width: 740px; min-width: 0; }
  h1 { font: 600 clamp(32px, 4.6vw, 44px)/1.1 var(--serif); letter-spacing: -0.012em; margin: 0 0 20px; text-wrap: balance; }
  h2 { font: 600 26px/1.2 var(--serif); margin: 52px 0 12px; padding-top: 4px; }
  h3 { font: 600 19px/1.3 var(--serif); margin: 34px 0 8px; }
  h2 a:hover::after, h3 a:hover::after { content: ' #'; color: var(--faint); }
  h1 + p { font-size: 18.5px; color: var(--muted); }
  p, ul, ol { margin: 0 0 16px; }
  ul, ol { padding-left: 24px; }
  li { margin: 5px 0; }
  li > p { margin-bottom: 8px; }
  li > ul, li > ol, li > .scroll { margin-top: 8px; }
  strong { font-weight: 600; }
  article p a, article li a, article td a { color: var(--accent); }
  article p a:hover, article li a:hover, article td a:hover { text-decoration: underline; }
  code { font: 0.88em var(--mono); background: var(--raised); padding: 2px 5px; border-radius: 5px; overflow-wrap: anywhere; }
  pre { margin: 0 0 20px; padding: 18px 20px; border: 1px solid var(--line); border-radius: 12px; background: var(--surface); font: 14px/1.7 var(--mono); overflow-x: auto; }
  pre code { background: none; padding: 0; font-size: inherit; overflow-wrap: normal; }
  .scroll { overflow-x: auto; margin: 0 0 20px; border: 1px solid var(--line); border-radius: 12px; }
  table { border-collapse: collapse; width: 100%; font-size: 15px; }
  th, td { text-align: left; vertical-align: top; padding: 10px 14px; border-bottom: 1px solid var(--line); }
  th { font-size: 12.5px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--faint); background: var(--surface); white-space: nowrap; }
  tr:last-child td { border-bottom: 0; }
  td:first-child { min-width: 150px; }

  .around { display: grid; grid-template-columns: 1fr 1fr; gap: 14px; margin-top: 56px; }
  .around a { border: 1px solid var(--line); border-radius: 12px; padding: 14px 16px; font: 600 17px/1.3 var(--serif); }
  .around a:hover { border-color: var(--muted); }
  .around a span { display: block; font: 600 12px/1.6 var(--sans); letter-spacing: 0.13em; text-transform: uppercase; color: var(--faint); }
  .around .next { text-align: right; }

  footer { margin-top: 72px; padding: 26px 0 56px; border-top: 1px solid var(--line); color: var(--faint); font-size: 14.5px; display: flex; gap: 8px 26px; flex-wrap: wrap; }
  footer a { color: var(--accent); }
  footer a:hover { text-decoration: underline; }

  @media (max-width: 860px) {
    .layout { grid-template-columns: 1fr; gap: 22px; padding-top: 20px; }
    .brand small { display: none; }
    .side { border: 1px solid var(--line); border-radius: 12px; background: var(--surface); }
    .side summary { display: block; padding: 12px 16px; font-weight: 600; cursor: pointer; }
    .side .inner { position: static; max-height: none; padding: 0 8px 12px; }
    .side a.sub { display: none; }
    h2 { margin-top: 40px; }
  }
`;

function html(current, titles) {
  const file = `${current.name}.md`;
  const name = current.name === 'README' ? 'Guides' : current.title;
  const lede = current.body.match(/<\/h1>\s*<p>([\s\S]*?)<\/p>/)?.[1].replace(/<[^>]+>/g, '') ?? '';
  return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>${esc(name)} · Spool</title>
<meta name="description" content="${esc(lede)}">
<link rel="icon" href="../assets/mark.svg">
<style>${STYLE}</style>
</head>
<body>
<!-- Made from docs/${file} by web/scripts/build-docs.mjs. Change that file, not this one. -->
<div class="wrap">
  <header>
    <a class="brand" href="../"><span class="mark"></span>Spool<small>Guides</small></a>
    <nav>
      <a href="./">All guides</a>
      <a href="${REPO}">GitHub</a>
    </nav>
  </header>
  <div class="layout">
    <aside class="side">
      <details open>
        <summary>Guides</summary>
        <nav class="inner" aria-label="Guides">${menu(current)}</nav>
      </details>
    </aside>
    <article>
${current.body}${around(current, titles)}
    </article>
  </div>
  <footer>
    <a href="${REPO}/blob/main/docs/${file}">Change this page on GitHub</a>
    <a href="../">Spool's front page</a>
    <span>Spool is free software under the GNU GPL v3.</span>
  </footer>
</div>
<script>if (matchMedia('(max-width: 860px)').matches) document.querySelector('.side details').open = false;</script>
</body>
</html>
`;
}

const names = readdirSync(docs).filter((f) => f.endsWith('.md')).map((f) => f.slice(0, -3)).sort();
for (const n of names) if (n !== 'README' && !ORDER.includes(n)) throw new Error(`docs/${n}.md is not in the menu in build-docs.mjs`);
for (const n of ORDER) if (!names.includes(n)) throw new Error(`the menu lists ${n}, and docs/${n}.md does not exist`);

const pages = names.map(render);
const byName = Object.fromEntries(pages.map((p) => [p.name, p]));
for (const p of pages) {
  for (const [target, anchor] of p.links) {
    const to = byName[target];
    if (!to) throw new Error(`${p.name}.md links to ${target}.md, which does not exist`);
    if (anchor && !to.headings.some((h) => h.id === anchor)) throw new Error(`${p.name}.md links to #${anchor} in ${target}.md, and no heading there has that name`);
  }
}

const titles = Object.fromEntries(pages.map((p) => [p.name, p.title]));
const files = Object.fromEntries(pages.map((p) => [`${page(p.name)}.html`, html(p, titles)]));

if (process.argv.includes('--check')) {
  const stale = Object.entries(files).filter(([f, text]) => !existsSync(join(out, f)) || readFileSync(join(out, f), 'utf8') !== text).map(([f]) => f);
  const extra = existsSync(out) ? readdirSync(out).filter((f) => !(f in files)) : [];
  if (stale.length || extra.length) {
    console.error(`docs/guide is out of date (${[...stale, ...extra].join(', ')}). Run: cd web && bun run docs`);
    process.exit(1);
  }
  console.log(`docs/guide matches the ${pages.length} guides`);
} else {
  mkdirSync(out, { recursive: true });
  for (const f of readdirSync(out)) if (!(f in files)) rmSync(join(out, f));
  for (const [f, text] of Object.entries(files)) writeFileSync(join(out, f), text);
  console.log(`wrote ${pages.length} pages to docs/guide`);
}
