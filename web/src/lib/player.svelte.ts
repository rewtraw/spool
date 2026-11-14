// What is playing, for the whole app: an album's tracks in order and which one, so music
// carries on while the library is looked through. The <audio> it plays through is in
// components/Player.svelte, which is always on the page.

export const player = $state({
  queue: [] as any[],
  at: 0,
  album: null as any,
  paused: true,
  now: 0,
  length: 0,
  audio: null as HTMLAudioElement | null,
});

// Browsers play FLAC, MP3, AAC, Opus and WAV themselves; anything else is made MP3 on the way.
const PLAYS = ['flac', 'mp3', 'm4a', 'mp4', 'aac', 'ogg', 'opus', 'wav'];
const src = (t: any) => `/api/music/tracks/${t.id}/play${PLAYS.includes(t.suffix) ? '' : '?format=mp3'}`;

export function playFrom(album: any, tracks: any[], i: number) {
  player.queue = tracks;
  player.album = album;
  player.at = i;
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
    player.paused = true;
    return;
  }
  player.at = to;
  start();
}

export function toggle() {
  const a = player.audio;
  if (!a || !player.queue.length) return;
  if (a.paused) a.play().catch(() => {});
  else a.pause();
}

export function seek(to: number) {
  if (player.audio) player.audio.currentTime = to;
}

/** Stop and put the bar away. */
export function stop() {
  player.audio?.pause();
  player.queue = [];
  player.album = null;
}
