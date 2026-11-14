// What is playing, for the whole app: an album's tracks in order and which one, so music
// carries on while the library is looked through. It plays here, through the <audio> in
// components/Player.svelte, or on a player in the house (a streamer or amplifier such as a
// WiiM), which Spool tells what to play and which fetches the music from Spool itself.
import { api } from './api';
import { toast } from './state.svelte';

/** A player in the house that music can be played on. */
export type Renderer = { id: string; name: string; model: string };

export const player = $state({
  queue: [] as any[],
  at: 0,
  album: null as any,
  paused: true,
  now: 0,
  length: 0,
  audio: null as HTMLAudioElement | null,
  /** Where music plays: null for here, or a player in the house. */
  on: null as Renderer | null,
  /** That player's volume, 0 to 100, when it says. */
  volume: null as number | null,
  /** What went wrong on that player, in words. */
  error: '',
  /** An audiobook being listened to, in place of an album: its files and chapters. */
  book: null as null | { id: number; title: string; authors: string[]; cover: string; files: { seconds: number }[]; chapters: { title: string; start: number }[]; seconds: number },
  /** How fast it is read, and when the sleep timer stops it (ms since 1970), or 0. */
  rate: 1,
  sleepAt: 0,
});

try {
  player.on = JSON.parse(localStorage.getItem('spool.playOn') ?? 'null');
} catch {}

// Browsers play FLAC, MP3, AAC, Opus and WAV themselves; anything else is made MP3 on the way.
const PLAYS = ['flac', 'mp3', 'm4a', 'mp4', 'aac', 'ogg', 'opus', 'wav'];
const src = (t: any) => `/api/music/tracks/${t.id}/play${PLAYS.includes(t.suffix) ? '' : '?format=mp3'}`;

/** Take in what a player in the house says it is doing. */
function heard(s: any) {
  if (!s) {
    player.queue = [];
    player.album = null;
    player.paused = true;
    return;
  }
  player.at = s.at;
  player.now = s.position;
  player.length = s.duration;
  player.paused = s.state !== 'playing' && s.state !== 'loading';
  player.volume = s.volume ?? player.volume;
  player.error = s.error ?? '';
  if (s.album && player.album?.id !== s.album.id) player.album = s.album;
  // Started from elsewhere, or before this page was open: what it is playing is shown, and
  // each track as it comes up, the rest being known only by id.
  const same = player.queue.length === s.queue.length && player.queue.every((t: any, i: number) => t.id === s.queue[i]);
  if (!same) player.queue = s.queue.map((id: number) => ({ id, title: '', artist: '', seconds: 0 }));
  if (s.track && !player.queue[s.at]?.title) player.queue[s.at] = s.track;
}

/** Counts each change of what plays where, so that an answer to an older question is let go. */
let turn = 0;

async function act(action: string, body: Record<string, unknown> = {}) {
  if (!player.on) return;
  const mine = turn;
  try {
    const s = await api.post(`/music/cast/${player.on.id}/${action}`, body);
    if (mine === turn) heard(s);
  } catch (e: any) {
    toast(e.message);
  }
}

// While music plays on a player in the house, keep up with it.
let following: ReturnType<typeof setInterval> | null = null;
function follow() {
  if (following) return;
  following = setInterval(async () => {
    if (!player.on) return stopFollowing();
    const mine = turn;
    const all = await api.get<any[]>('/music/cast').catch(() => null);
    if (!all || mine !== turn || !player.on) return;
    const s = all.find((x) => x.renderer === player.on?.id);
    heard(s ?? null);
    if (!s) stopFollowing();
  }, 1500);
}
function stopFollowing() {
  if (following) clearInterval(following);
  following = null;
}

export async function playFrom(album: any, tracks: any[], i: number) {
  if (player.book) stopBook();
  const mine = ++turn;
  player.queue = tracks;
  player.album = album;
  player.at = i;
  if (player.on) {
    player.audio?.pause();
    try {
      const s = await api.post('/music/cast', { renderer: player.on.id, tracks: tracks.map((t) => t.id), at: i, album_id: album?.id });
      if (mine !== turn) return;
      heard(s);
      follow();
    } catch (e: any) {
      toast(e.message);
    }
    return;
  }
  start();
}

function start() {
  const a = player.audio;
  const t = player.queue[player.at];
  if (!a || !t) return;
  a.src = src(t);
  a.play().catch(() => {});
}

export function step(by = 1) {
  const to = player.at + by;
  if (to < 0 || to >= player.queue.length) {
    if (!player.on) player.paused = true;
    return;
  }
  if (player.on) return void act(by > 0 ? 'next' : 'previous');
  player.at = to;
  start();
}

export function toggle() {
  if (player.on && !player.book) return void act(player.paused ? 'play' : 'pause');
  const a = player.audio;
  if (!a || (!player.queue.length && !player.book)) return;
  if (a.paused) a.play().catch(() => {});
  else a.pause();
}

export function seek(to: number) {
  if (player.on) {
    player.now = to;
    return void act('seek', { seconds: to });
  }
  if (player.audio) player.audio.currentTime = to;
}

export function setVolume(v: number) {
  player.volume = v;
  void act('volume', { volume: Math.round(v) });
}

/** Stop and put the bar away. */
export function stop() {
  if (player.on) void act('stop');
  turn++;
  stopFollowing();
  player.audio?.pause();
  player.queue = [];
  player.album = null;
}

/** Play on a player in the house, or here; what is playing goes along, from where it was. */
export async function playOn(r: Renderer | null) {
  if (player.on?.id === r?.id) return;
  turn++;
  const was = player.on;
  // What is playing, or paused, goes along, from the same moment.
  const going = player.queue.length ? { tracks: player.queue, at: player.at, album: player.album, now: player.now, paused: player.paused } : null;
  if (was) {
    await api.post(`/music/cast/${was.id}/stop`).catch(() => {});
    stopFollowing();
  } else {
    player.audio?.pause();
  }
  player.on = r;
  player.volume = null;
  player.error = '';
  try {
    localStorage.setItem('spool.playOn', JSON.stringify(r));
  } catch {}
  if (!going) return;
  if (!r && going.paused) {
    // Back here, paused: ready at the same moment, waiting for Play.
    player.queue = going.tracks;
    player.album = going.album;
    player.at = going.at;
    const a = player.audio;
    const t = going.tracks[going.at];
    if (a && t) {
      a.src = src(t);
      a.currentTime = going.now;
    }
    player.paused = true;
    return;
  }
  await playFrom(going.album, going.tracks, going.at);
  // Carry on from the same moment, paused if it was. A player in the house needs a moment to begin.
  setTimeout(() => {
    if (going.now > 2) seek(going.now);
    if (going.paused && r) void act('pause');
  }, r ? 2500 : 300);
}

/** Pick up what a player in the house is already playing, when this page opens. */
export async function resume() {
  if (!player.on) return;
  const all = await api.get<any[]>('/music/cast').catch(() => null);
  const s = all?.find((x) => x.renderer === player.on?.id);
  if (s) {
    heard(s);
    follow();
  }
}

// ---- audiobooks: one book's files played end to end, with the place kept per person

/** Where in the book a file begins. */
const fileStart = (n: number) => (player.book?.files ?? []).slice(0, n).reduce((t, f) => t + f.seconds, 0);

/** The file, and the moment in it, that a place in the book falls in. */
function locate(at: number): [number, number] {
  const files = player.book?.files ?? [];
  let start = 0;
  for (let n = 0; n < files.length; n++) {
    if (at < start + files[n].seconds || n === files.length - 1) return [n, Math.max(0, at - start)];
    start += files[n].seconds;
  }
  return [0, 0];
}

let bookFile = 0;
let saving: ReturnType<typeof setInterval> | null = null;
/** Counts each book started, seek and stop, so that what was asked for before is let go. */
let bookTurn = 0;
/** Set once a book has found its place; until then nothing is saved for it. */
let bookReady = false;
let pendingLoad: (() => void) | null = null;

/** The chapter a place in the book is in. */
export function chapterAt(at: number) {
  const ch = player.book?.chapters ?? [];
  let i = 0;
  while (i + 1 < ch.length && ch[i + 1].start <= at + 0.5) i++;
  return i;
}

function saveBook() {
  if (!player.book || !bookReady) return;
  void api.put(`/books/${player.book.id}/listen`, { position: player.now, rate: player.rate }).catch(() => {});
}

function loadBookAt(at: number, play: boolean) {
  const a = player.audio;
  if (!a || !player.book) return;
  const [n, off] = locate(at);
  bookFile = n;
  const mine = ++bookTurn;
  // A load still waiting for its file is overtaken by this one.
  if (pendingLoad) a.removeEventListener('loadedmetadata', pendingLoad);
  pendingLoad = null;
  const src = `/api/books/${player.book.id}/audio/${n}`;
  if (!a.src.endsWith(src)) {
    const ready = () => {
      pendingLoad = null;
      if (mine !== bookTurn || !player.book) return;
      a.currentTime = off;
      a.playbackRate = player.rate;
      if (play) a.play().catch(() => {});
    };
    pendingLoad = ready;
    a.addEventListener('loadedmetadata', ready, { once: true });
    a.src = src;
  } else {
    a.currentTime = off;
    if (play) a.play().catch(() => {});
  }
  player.now = at;
}

/** Listen to an audiobook from where whoever is signed in left off, or from `at`. */
export async function playBook(book: any, at?: number) {
  if (player.on) {
    // Players in the house take albums for now; an audiobook plays here.
    toast(`Audiobooks play on this device for now, not on ${player.on.name}.`);
  }
  turn++;
  stopFollowing();
  // The book being left keeps its place before anything about it changes.
  if (player.book) {
    saveBook();
    player.audio?.pause();
  }
  bookReady = false;
  const mine = ++bookTurn;
  const a = book.audio;
  player.queue = [];
  player.album = null;
  player.book = { id: book.id, title: book.title, authors: book.authors ?? [], cover: book.cover, files: a.files, chapters: a.chapters, seconds: a.seconds };
  const kept = at == null ? await api.get<{ position: number; rate: number }>(`/books/${book.id}/listen`).catch(() => null) : null;
  // Another book, or a stop, came while this one was being looked up.
  if (mine !== bookTurn || player.book?.id !== book.id) return;
  player.rate = kept?.rate ?? player.rate;
  player.length = a.seconds;
  // Within a few seconds of the end is finished: start again.
  const from = at ?? (kept && kept.position < a.seconds - 10 ? kept.position : 0);
  loadBookAt(from, true);
  bookReady = true;
  if (!saving) saving = setInterval(() => !player.paused && saveBook(), 10_000);
}

/** The audio element reached the end of a file: on to the next, or done. */
export function bookEnded() {
  if (!player.book) return;
  if (bookFile + 1 < player.book.files.length) loadBookAt(fileStart(bookFile + 1), true);
  else {
    player.paused = true;
    saveBook();
  }
}

/** The audio element moved on: where that is in the book, and the sleep timer. */
export function bookTime() {
  // Only once the book's own file is loaded; until then the element may still be the last book's.
  if (!player.book || !player.audio || pendingLoad || !player.audio.src.includes(`/api/books/${player.book.id}/audio/`)) return;
  player.now = fileStart(bookFile) + player.audio.currentTime;
  if (player.sleepAt && Date.now() >= player.sleepAt) {
    player.sleepAt = 0;
    player.audio.pause();
    saveBook();
  }
}

export function bookSeek(to: number) {
  if (!player.book) return;
  loadBookAt(Math.max(0, Math.min(to, player.book.seconds - 1)), !player.paused);
}

export function bookSkip(by: number) {
  bookSeek(player.now + by);
}

export function bookChapter(by: number) {
  if (!player.book) return;
  const ch = player.book.chapters;
  const i = chapterAt(player.now);
  // Back goes to the start of this chapter first, unless just past it.
  const to = by < 0 && player.now - (ch[i]?.start ?? 0) > 3 ? i : Math.max(0, Math.min(ch.length - 1, i + by));
  bookSeek(ch[to]?.start ?? 0);
}

export function setRate(r: number) {
  player.rate = r;
  if (player.audio) player.audio.playbackRate = r;
  saveBook();
}

/** Stop after so many minutes, or at the end of this chapter (0). Null turns it off. */
export function sleepIn(minutes: number | null) {
  if (minutes == null) return void (player.sleepAt = 0);
  if (minutes === 0 && player.book) {
    const ch = player.book.chapters;
    const next = ch[chapterAt(player.now) + 1]?.start ?? player.book.seconds;
    player.sleepAt = Date.now() + ((next - player.now) / player.rate) * 1000;
  } else player.sleepAt = Date.now() + minutes * 60_000;
}

export function stopBook() {
  saveBook();
  bookTurn++;
  bookReady = false;
  if (pendingLoad) player.audio?.removeEventListener('loadedmetadata', pendingLoad);
  pendingLoad = null;
  player.audio?.pause();
  player.book = null;
  player.sleepAt = 0;
  if (saving) clearInterval(saving);
  saving = null;
}

if (typeof window !== 'undefined') {
  // Keep the place when the page is put away or closed.
  window.addEventListener('pagehide', () => saveBook());
  document.addEventListener('visibilitychange', () => document.visibilityState === 'hidden' && saveBook());
}
