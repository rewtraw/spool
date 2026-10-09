const QUALITY: Record<string, [string, string]> = {};

export function setQualities(list: { key: string; movie: string; tv: string }[]) {
  for (const q of list) QUALITY[q.key] = [q.movie, q.tv];
}

// Names for the fixed set of qualities, used until (or if never) the server's list arrives, so a
// label never shows a raw key such as "bluray-1080p".
function builtinName(key: string, kind: 'movie' | 'series'): string {
  const [source, res] = key.split('-');
  if (source === 'remux') return kind === 'movie' ? `Remux-${res}` : `Bluray-${res} Remux`;
  const sources: Record<string, string> = { bluray: 'Bluray', webdl: 'WEBDL', webrip: 'WEBRip', hdtv: 'HDTV' };
  if (sources[source] && res) return `${sources[source]}-${res}`;
  const fixed: Record<string, string> = { 'dvd-r': 'DVD-R', 'br-disk': 'BR-DISK', 'raw-hd': 'Raw-HD', unknown: 'Unknown' };
  return fixed[key] ?? key.toUpperCase();
}

export function qualityName(q: { quality: string; revision?: { version: number; real: number; is_repack: boolean } } | null | undefined, kind: 'movie' | 'series' = 'movie'): string {
  if (!q) return '';
  const names = QUALITY[q.quality];
  let s = names ? names[kind === 'movie' ? 0 : 1] : builtinName(q.quality, kind);
  if (q.revision?.is_repack) s += ' Repack';
  else if ((q.revision?.version ?? 1) > 1) s += ' Proper';
  return s;
}

export function bytes(n: number | null | undefined): string {
  if (!n) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let i = 0;
  let v = n;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v >= 100 || i === 0 ? v.toFixed(0) : v.toFixed(1)} ${units[i]}`;
}

export function speed(n: number): string {
  return n > 0 ? `${bytes(n)}/s` : '';
}

export function duration(secs: number | null | undefined): string {
  if (secs == null) return '';
  if (secs < 60) return `${Math.max(1, Math.round(secs))} sec`;
  if (secs < 3600) return `${Math.round(secs / 60)} min`;
  const h = Math.floor(secs / 3600);
  const m = Math.round((secs % 3600) / 60);
  return m ? `${h} hr ${m} min` : `${h} hr`;
}

export function ago(ts: number): string {
  if (!ts) return '';
  const s = Date.now() / 1000 - ts;
  if (s < 60) return 'just now';
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `${Math.floor(s / 3600)} hr ago`;
  if (s < 86400 * 30) return `${Math.floor(s / 86400)} days ago`;
  return new Date(ts * 1000).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' });
}

export function age(published: number): string {
  if (!published) return '';
  const d = (Date.now() / 1000 - published) / 86400;
  if (d < 1) return `${Math.max(1, Math.round(d * 24))} hr`;
  if (d < 365) return `${Math.round(d)} days`;
  return `${(d / 365).toFixed(1)} yr`;
}

export function day(date: string | null | undefined): string {
  if (!date) return '';
  const d = new Date(date.slice(0, 10) + 'T12:00:00');
  return d.toLocaleDateString(undefined, { weekday: 'short', month: 'short', day: 'numeric' });
}

export function fullDate(date: string | null | undefined): string {
  if (!date) return '';
  const d = new Date(date.slice(0, 10) + 'T12:00:00');
  return d.toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' });
}

export function pct(done: number, total: number): number {
  return total > 0 ? Math.min(100, (done / total) * 100) : 0;
}

export const STATE_LABEL: Record<string, string> = {
  queued: 'Queued',
  downloading: 'Downloading',
  paused: 'Paused',
  verifying: 'Verifying',
  repairing: 'Repairing',
  extracting: 'Extracting',
  finishing: 'Finishing',
  completed: 'Downloaded',
  failed: 'Failed',
  importing: 'Importing',
  imported: 'In library',
  import_blocked: 'Needs attention',
  cancelled: 'Removed',
};
