// Thin client for the Spool API.

export class ApiError extends Error {
  constructor(public status: number, message: string) {
    super(message);
  }
}

// ---- button feedback
// Any button whose click starts a request is marked busy until that request settles: it shows a
// spinner, and further clicks on it are swallowed, so an impatient second click can never send
// the same action twice. This lives here, not in each view, so every button gets it for free.
let clicked: HTMLElement | null = null;
if (typeof document !== 'undefined') {
  document.addEventListener(
    'click',
    (e) => {
      const el = (e.target as HTMLElement | null)?.closest?.('button, [role="button"]') as HTMLElement | null;
      if (!el) return;
      if (el.dataset.busy) {
        e.preventDefault();
        e.stopImmediatePropagation();
        return;
      }
      clicked = el;
      // Cleared once the click handler's synchronous part has run (a confirm dialog blocks this too).
      setTimeout(() => {
        if (clicked === el) clicked = null;
      }, 0);
    },
    true,
  );
}

function markBusy(): () => void {
  const el = clicked;
  if (!el) return () => {};
  el.dataset.busy = String(Number(el.dataset.busy ?? 0) + 1);
  el.setAttribute('aria-busy', 'true');
  return () => {
    const n = Number(el.dataset.busy ?? 1) - 1;
    if (n > 0) el.dataset.busy = String(n);
    else {
      delete el.dataset.busy;
      el.removeAttribute('aria-busy');
    }
  };
}

async function request<T>(method: string, path: string, body?: unknown, raw?: BodyInit): Promise<T> {
  const done = markBusy();
  try {
    return await send<T>(method, path, body, raw);
  } finally {
    done();
  }
}

async function send<T>(method: string, path: string, body?: unknown, raw?: BodyInit): Promise<T> {
  const init: RequestInit = { method, credentials: 'same-origin', headers: {} };
  if (raw !== undefined) {
    init.body = raw;
  } else if (body !== undefined) {
    (init.headers as Record<string, string>)['content-type'] = 'application/json';
    init.body = JSON.stringify(body);
  }
  let res: Response;
  try {
    res = await fetch(`/api${path}`, init);
  } catch {
    throw new ApiError(0, 'Spool is not reachable');
  }
  const text = await res.text();
  let data: any = null;
  try {
    data = text ? JSON.parse(text) : null;
  } catch {
    /* not json */
  }
  if (!res.ok) {
    if (res.status === 401 && path !== '/login') window.dispatchEvent(new Event('spool:signin'));
    throw new ApiError(res.status, data?.error ?? `Request failed (${res.status})`);
  }
  return data as T;
}

export const api = {
  get: <T = any>(p: string) => request<T>('GET', p),
  post: <T = any>(p: string, b?: unknown) => request<T>('POST', p, b ?? {}),
  put: <T = any>(p: string, b?: unknown) => request<T>('PUT', p, b ?? {}),
  patch: <T = any>(p: string, b?: unknown) => request<T>('PATCH', p, b ?? {}),
  del: <T = any>(p: string) => request<T>('DELETE', p),
  upload: <T = any>(p: string, file: Blob) => request<T>('POST', p, undefined, file),
};

export type Kind = 'movie' | 'series';

export interface Quality {
  quality: string;
  revision?: { version: number; real: number; is_repack: boolean };
}

export interface Title {
  original_language?: string | null;
  id: number;
  kind: Kind;
  title: string;
  sort_title: string;
  year: number;
  overview: string;
  status: string;
  runtime: number;
  poster?: string | null;
  fanart?: string | null;
  genres: string[];
  monitored: boolean;
  profile_id: number;
  profile?: string | null;
  path: string;
  added_at: number;
  imdb_id?: string | null;
  tmdb_id?: number | null;
  tvdb_id?: number | null;
  tvmaze_id?: number | null;
  network?: string | null;
  studio?: string | null;
  in_cinemas?: string | null;
  digital_release?: string | null;
  physical_release?: string | null;
  minimum_availability: string;
  seasons: { number: number; monitored: boolean }[];
  file_count: number;
  size: number;
  episodes_aired: number;
  episodes_have: number;
  active?: string | null;
  available: boolean;
  library_id?: number | null;
}

export interface Episode {
  id: number;
  season: number;
  episode: number;
  title: string;
  overview: string;
  air_date?: string | null;
  air_date_utc?: string | null;
  monitored: boolean;
  file_id?: number | null;
}

export interface MediaFile {
  id: number;
  rel_path: string;
  size: number;
  quality: Quality;
  release_group?: string | null;
  edition: string;
  media_info?: { video_codec: string; audio_codec: string; audio_channels: number; dynamic_range_type: string; width: number; height: number; runtime_secs: number } | null;
  scene_name?: string | null;
  added_at: number;
}

export interface Release {
  guid: string;
  title: string;
  indexer: string;
  size: number;
  published: number;
  info_url: string;
}

export interface Decision {
  id: number;
  ts: number;
  title_id: number;
  release: Release;
  quality: Quality;
  languages: string[];
  release_group?: string | null;
  covers: string;
  accepted: boolean;
  rejections: { code: string; message: string }[];
  source: string;
}

export interface Job {
  id: string;
  name: string;
  state: string;
  total_bytes: number;
  done_bytes: number;
  speed: number;
  eta_secs?: number | null;
  missing_articles: number;
  damaged_articles: number;
  message: string;
  error?: string | null;
}

export interface Acquisition {
  id: number;
  title_id: number;
  episode_ids: number[];
  state: string;
  release: Release;
  quality: Quality;
  job_id?: string | null;
  error?: string | null;
  reason: string;
  created_at: number;
  updated_at: number;
}
