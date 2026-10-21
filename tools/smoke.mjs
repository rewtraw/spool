#!/usr/bin/env bun
// Open every page of a running Spool in a real browser and check that it shows what it should.
//
//   bun tools/smoke.mjs http://localhost:7979             check every page
//   bun tools/smoke.mjs http://localhost:7979 --shots DIR  and keep a picture of each
//   bun tools/smoke.mjs --shot URL OUT.png [WIDTHxHEIGHT] [SCRIPT]  one picture of one page,
//                                                          after running SCRIPT in it if given
//
// The tests in crates/ check what Spool records and answers. This checks what a person
// sees: that each page loads its data, draws, and throws no error while doing it. It drives
// Chrome over its debugging port, because a page here is drawn after its data arrives and
// Chrome's own --screenshot fires before that.
//
// Needs Chrome or Chromium (CHROME names it if it is somewhere unusual). An instance with an
// app password is signed in to with its key, from SPOOL_API_KEY. Pages that show keys
// (General, Library, Usenet, Indexers) are checked and never pictured.

import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const key = process.env.SPOOL_API_KEY;
const headers = key ? { 'X-Api-Key': key } : {};

const args = process.argv.slice(2);
const flag = (name) => { const i = args.indexOf(name); return i < 0 ? null : args.splice(i, 2)[1]; };
const one = args[0] === '--shot' ? args.slice(1) : null;
const shots = flag('--shots');
const base = (one ? null : args[0])?.replace(/\/$/, '');
if (!one && !base) {
  console.error('usage: bun tools/smoke.mjs <address of a running Spool> [--shots DIR]\n       bun tools/smoke.mjs --shot URL OUT.png [WIDTHxHEIGHT] [SCRIPT]');
  process.exit(2);
}

const chrome = [process.env.CHROME, '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', '/Applications/Chromium.app/Contents/MacOS/Chromium', '/usr/bin/google-chrome', '/usr/bin/chromium', '/usr/bin/chromium-browser'].find((p) => p && existsSync(p));
if (!chrome) {
  console.error('No Chrome or Chromium found. Set CHROME to its path.');
  process.exit(2);
}
const port = 9300 + Math.floor(Math.random() * 600);
const profile = mkdtempSync(join(tmpdir(), 'spool-smoke-'));
const browser = spawn(chrome, ['--headless=new', `--user-data-dir=${profile}`, `--remote-debugging-port=${port}`, '--hide-scrollbars', '--no-first-run', 'about:blank'], { stdio: 'ignore' });
const done = (code) => { browser.kill(); try { rmSync(profile, { recursive: true, force: true }); } catch {} process.exit(code); };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
for (let i = 0; ; i++) {
  try { await fetch(`http://127.0.0.1:${port}/json/version`); break; } catch { if (i > 60) { console.error('Chrome did not start.'); done(2); } await sleep(150); }
}

/** A tab, with what went wrong in it noted as it happens. */
async function open(width = 1280, height = 1400) {
  const tab = await (await fetch(`http://127.0.0.1:${port}/json/new?about:blank`, { method: 'PUT' })).json();
  const ws = new WebSocket(tab.webSocketDebuggerUrl);
  let id = 0;
  const waiting = new Map();
  const problems = [];
  ws.onmessage = (e) => {
    const m = JSON.parse(e.data);
    if (m.id && waiting.has(m.id)) { waiting.get(m.id)(m.result ?? {}); waiting.delete(m.id); }
    if (m.method === 'Runtime.exceptionThrown') problems.push(`error: ${m.params.exceptionDetails.exception?.description ?? m.params.exceptionDetails.text}`.split('\n')[0]);
    if (m.method === 'Log.entryAdded' && m.params.entry.level === 'error') problems.push(`console: ${m.params.entry.text} ${m.params.entry.url ?? ''}`.trim());
    if (m.method === 'Network.responseReceived' && m.params.response.status >= 500) problems.push(`HTTP ${m.params.response.status} from ${m.params.response.url}`);
  };
  await new Promise((r) => (ws.onopen = r));
  const send = (method, params = {}) => new Promise((r) => { waiting.set(++id, r); ws.send(JSON.stringify({ id, method, params })); });
  for (const d of ['Runtime', 'Log', 'Network', 'Page']) await send(`${d}.enable`);
  if (key) await send('Network.setExtraHTTPHeaders', { headers });
  // A picture as narrow as a phone is taken as a phone sees it: a touch screen, with no pointer to hover.
  const phone = width <= 500;
  await send('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: phone ? 2 : 1, mobile: phone });
  if (phone) await send('Emulation.setTouchEmulationEnabled', { enabled: true, maxTouchPoints: 5 });
  const text = async () => (await send('Runtime.evaluate', { expression: 'document.body.innerText', returnByValue: true })).result?.value ?? '';
  return {
    problems,
    /** Go to a page and wait until it has stopped saying it is loading. */
    async go(url) {
      problems.length = 0;
      await send('Page.navigate', { url });
      let seen = '';
      for (let i = 0; i < 40; i++) {
        await sleep(250);
        seen = await text();
        if (seen.length > 40 && !/Loading…|Searching…/.test(seen)) break;
      }
      await sleep(400);
      return text();
    },
    /** Run something in the page, wait for what it starts, and give back what it answered. */
    async run(expression) {
      const r = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
      await sleep(900);
      return r.result?.value;
    },
    async picture(file) {
      const shot = await send('Page.captureScreenshot', { format: 'png' });
      await Bun.write(file, Buffer.from(shot.data, 'base64'));
    },
    async close() { ws.close(); await fetch(`http://127.0.0.1:${port}/json/close/${tab.id}`).catch(() => {}); },
  };
}

if (one) {
  const [url, file, size = '1280x1400', script] = one;
  const [w, h] = size.split('x').map(Number);
  const tab = await open(w, h);
  await tab.go(url);
  if (script) { const said = await tab.run(script); if (said !== undefined) console.log(said); }
  await tab.picture(file);
  console.log(`wrote ${file}`);
  done(0);
}

const api = async (path) => { try { const r = await fetch(base + path, { headers }); return r.ok ? r.json() : null; } catch { return null; } };
const status = await api('/api/status');
if (!status) {
  console.error(`${base} did not answer as Spool does, or it has a password and SPOOL_API_KEY does not hold its key.`);
  done(2);
}
const listed = await api('/api/titles');
const titles = listed?.titles ?? listed ?? [];
const collections = (await api('/api/collections')) ?? [];
const moves = (await api('/api/activity'))?.moves ?? {};
// A move that is under way, or waiting, has to be on show where activity is.
const moving = moves.now || moves.waiting?.length ? ['Moving to another location', ...(moves.now?.title ? [moves.now.title.slice(0, 12)] : []), ...(moves.waiting?.length ? ['waiting'] : [])] : [];

// Each page, what must be on it, and whether it may be pictured.
const pages = [
  ['home', '#/home', ['Home', ...(moving.length ? ['In progress'] : [])], true],
  ['library', '#/library', ['Library', ...(titles.length ? [titles[0].title.slice(0, 12)] : [])], true],
  ['discover', '#/discover', ['Discover'], true],
  ['activity', '#/activity', ['Activity', ...moving], true],
  ['clips', '#/clips', ['Clips', 'Make a clip', 'Find'], true],
  ['archive', '#/archive', ['Archive'], true],
  ['settings-general', '#/settings/general', ['Mode', 'Downloads', 'Connections'], false],
  ['settings-players', '#/settings/players', ['People', 'Asked for', 'Lately'], true],
  ['settings-library', '#/settings/library', ['This machine', 'Add a location'], false],
  ['settings-usenet', '#/settings/usenet', ['Speed'], false],
  ['settings-indexers', '#/settings/indexers', [/Add an indexer/], false],
  ['settings-profiles', '#/settings/profiles', ['Hard size limits'], true],
  ['settings-naming', '#/settings/naming', ['Renaming'], true],
  ['settings-plex', '#/settings/plex', [/Library match|Plex is not set up|Plex is turned off/], true],
  ['settings-compact', '#/settings/compact', ['How they are made', 'The library'], true],
  ['settings-space', '#/settings/space', ['Free'], true],
  ['settings-maintenance', '#/settings/maintenance', ['Read indexer feeds', 'Recycled files'], true],
];
const film = titles.find((t) => t.kind === 'movie' && t.file_count > 0) ?? titles[0];
if (film) pages.push(['title', `#/title/${film.id}`, [film.title.slice(0, 12), 'Monitored', 'Make a clip'], true]);
if (collections[0]) pages.push(['collection', `#/collection/${collections[0].id}`, [collections[0].name.slice(0, 12), 'in the library'], true]);

if (shots) mkdirSync(shots, { recursive: true });
const tab = await open();
let failed = 0;
for (const [name, route, must, mayPicture] of pages) {
  const seen = await tab.go(`${base}/${route}`);
  // Headings are drawn in capitals by the style sheet, so letter case is not compared.
  const missing = must.filter((m) => (m instanceof RegExp ? !m.test(seen) : !seen.toLowerCase().includes(m.toLowerCase()))).map(String);
  const wrong = [...missing.map((m) => `does not show ${m}`), ...tab.problems];
  if (shots && mayPicture) await tab.picture(join(shots, `${name}.png`));
  console.log(`${wrong.length ? 'FAIL' : 'ok  '} ${name}${wrong.length ? `\n       ${wrong.join('\n       ')}` : ''}`);
  failed += wrong.length ? 1 : 0;
}
await tab.close();
console.log(failed ? `\n${failed} of ${pages.length} pages have a problem.` : `\nAll ${pages.length} pages load and draw.`);
done(failed ? 1 : 0);
