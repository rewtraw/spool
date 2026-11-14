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
  if (player.on) return void act(player.paused ? 'play' : 'pause');
  const a = player.audio;
  if (!a || !player.queue.length) return;
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
