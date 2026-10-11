#!/usr/bin/env bun
// Start a Spool on a made-up library, with a move under way, and look at it.
//
//   bun tools/demo.mjs                 start it, check every page, and stop
//   bun tools/demo.mjs --shots DIR     and keep a picture of each page
//   bun tools/demo.mjs --stay          leave it running to look at by hand (ctrl-c stops it)
//   bun tools/demo.mjs --port 7990     on a port other than 7980
//
// The library is laid out by `spool demo` in a folder of its own, away from any real one,
// and is removed afterwards. It needs no keys and reaches no network. One film is set
// moving to another location, slowly, with others in line behind it, so that what a page
// shows while something is under way can be seen. Build first: `cargo build -p spool`
// after `bun run build` in web/.

import { spawn, spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, readdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';

const args = process.argv.slice(2);
const flag = (name) => { const i = args.indexOf(name); return i < 0 ? null : args.splice(i, 2)[1]; };
const shots = flag('--shots');
const port = flag('--port') ?? '7980';
const stay = args.includes('--stay');
const repo = dirname(import.meta.dir);
const bin = [process.env.SPOOL_BIN, join(repo, 'target/debug/spool'), join(repo, 'target/release/spool')].find((p) => p && existsSync(p));
if (!bin) {
  console.error('No spool program found. Run `cargo build -p spool` first, or set SPOOL_BIN.');
  process.exit(2);
}

const root = mkdtempSync(join(tmpdir(), 'spool-demo-'));
const data = join(root, 'data');
const base = `http://127.0.0.1:${port}`;
let server;
const stop = (code) => {
  server?.kill();
  try { rmSync(root, { recursive: true, force: true }); } catch {}
  process.exit(code);
};
process.on('SIGINT', () => stop(0));

const seeded = spawnSync(bin, ['--data-dir', data, 'demo', root], { encoding: 'utf8' });
if (seeded.status !== 0) {
  console.error(seeded.stderr || seeded.stdout);
  stop(2);
}
console.log(seeded.stdout.trim());

// No scheduled work, so nothing goes looking for the made-up titles; and a copy held to a
// pace, so the move lasts long enough to be looked at.
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const get = async (path) => (await fetch(base + path)).json();
const post = async (path, body) => (await fetch(base + path, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) })).json();
async function start() {
  server = spawn(bin, ['--data-dir', data, 'serve', '--bind', `127.0.0.1:${port}`], { stdio: 'ignore', env: { ...process.env, SPOOL_NO_SCHEDULER: '1', SPOOL_MOVE_PACE: String(2 << 20) } });
  for (let i = 0; ; i++) {
    try { await get('/api/status'); break; } catch { if (i > 80) { console.error('Spool did not start.'); stop(2); } await sleep(150); }
  }
}
await start();

const titles = await get('/api/titles');
const named = (name) => (titles.titles ?? titles).find((t) => t.title === name)?.id;
const shelf = (await get('/api/locations')).find((l) => l.name === 'Shelf')?.id;
// The big one first, so it is the one under way; then two to wait behind it.
const asked = await post('/api/titles/move', { ids: [named('The General')], location: shelf });
const more = await post('/api/titles/move', { ids: [named('Metropolis'), named('Nosferatu')], location: shelf });
if (asked.queued == null || more.queued == null) {
  console.error(`The moves were refused: ${JSON.stringify([asked, more])}`);
  stop(2);
}
console.log(`moving ${asked.queued + more.queued} titles to Shelf, slowly`);
await sleep(4000);
if (!(await get('/api/activity')).moves?.now) {
  console.error('No move is under way, so there is nothing of it to look at.');
  stop(2);
}

if (stay) {
  console.log(`Spool is at ${base}. Ctrl-c stops it and removes the library.`);
  await new Promise(() => {});
}
const smoke = spawnSync('bun', [join(repo, 'tools/smoke.mjs'), base, ...(shots ? ['--shots', shots] : [])], { stdio: 'inherit' });

// Then what cannot be seen in a picture: that a move stopped halfway, by a person or by a
// restart, leaves the library whole.
let wrong = 0;
const check = (ok, what) => { console.log(`${ok ? 'ok  ' : 'FAIL'} ${what}`); if (!ok) wrong++; };
const halfMade = () => readdirSync(join(root, 'Shelf/Films')).filter((n) => n.startsWith('.spool-moving-'));
const moves = async () => (await get('/api/activity')).moves;
const until = async (test, seconds = 20) => { for (let i = 0; i < seconds * 4; i++) { if (await test()) return true; await sleep(250); } return false; };

check(halfMade().length === 1, 'a move under way copies under a working name');
const said = await post('/api/titles/move/cancel', { ids: [named('The General')] });
check(said.stopping === true, 'the move under way can be told to stop');
check(await until(async () => (await moves()).now?.title !== 'The General'), 'and it stops');
check(existsSync(join(root, 'Movies/The General (1926)/The General (1926) Bluray-1080p.mkv')) && !existsSync(join(root, 'Shelf/Films/The General (1926)')), 'a cancelled title is still where it was, and not where it was going');
check(await until(async () => halfMade().every((n) => !n.endsWith(`-${named('The General')}`))), 'what was copied of it is removed');

// The next in line is now under way. Stop Spool dead in the middle of it.
check(await until(async () => (await moves()).now?.total > 0 && halfMade().length === 1), 'the next in line starts by itself');
const interrupted = (await moves()).now?.title;
server.kill('SIGKILL');
await sleep(500);
check(halfMade().length === 1, 'a restart in the middle of a move leaves a half-made copy');
await start();
check(await until(async () => { const m = await moves(); return !m.now && !m.waiting.length; }, 40), 'after the restart the line is taken up again and finished');
const kept = (name) => readdirSync(join(root, 'Shelf/Films')).some((n) => n.startsWith(name)) && !readdirSync(join(root, 'Movies')).some((n) => n.startsWith(name));
check(kept('Metropolis') && kept('Nosferatu'), `both titles are on Shelf and gone from where they were (${interrupted} was the one interrupted)`);
check(halfMade().length === 0, 'and no half-made copy is left');
console.log(wrong ? `\n${wrong} checks of moving failed.` : '\nMoves stop, resume and clean up as they should.');
stop((smoke.status ?? 1) || (wrong ? 1 : 0));
