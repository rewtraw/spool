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
  del: <T = any>(p: string, b?: unknown) => request<T>('DELETE', p, b),
  upload: <T = any>(p: string, file: Blob) => request<T>('POST', p, undefined, file),
};

export type Kind = 'movie' | 'series';

export interface Quality {
  quality: string;
  revision?: { version: number; real: number; is_repack: boolean };
}

/** Titles that belong together: a series of films, an anime's series, films and specials, or a set of your own. */
export interface Collection {
  id: number;
  name: string;
  poster?: string | null;
  kind: 'anime' | 'films' | 'own';
  /** In order of release. `role` says what a member is when not simply part of the story; `story` is its place in the story's own order. */
  members: { title_id: number; year: number; role: string; story: number; watched: boolean }[];
  /** Works of the same story, or films of the same collection, that are not in the library. */
  missing: { anilist_id: number; tmdb_id?: number | null; title: string; year: number; format: string; episodes: number; role: string; poster?: string | null; story: number; can_add: boolean }[];
  /** The member to watch next, once one has been watched. */
  next?: number | null;
  /** Its titles have been put in an order of your own, which `story` then follows. */
  ordered?: boolean;
}

/** Where the person signed in stands with a film, an episode or a series. */
export interface Watched {
  done: boolean;
  /** How much of it is seen, from 0 to 1: of a film or an episode its running time, of a series its episodes. */
  part?: number;
  /** Seconds in, for a film or an episode stopped part-way. */
  position?: number;
  runtime?: number;
}

export interface Title {
  /** Where the person signed in stands with it; absent when untouched. */
  watched?: Watched;
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
  /** The location the title is kept on, when there is more than one. */
  location?: string | null;
  /** The other locations that hold a copy of it. */
  also_on?: string[];
  /** Its location cannot be reached right now. */
  offline?: boolean;
  /** Its location is one Spool only reads. */
  read_only?: boolean;
  /** Being moved to another location, or waiting its turn. */
  moving?: boolean;
  /** How far the move has got, on the title's own page. */
  move?: { to: string; done: number; total: number } | null;
  move_waiting?: boolean;
  /** A film that is posted as its episodes, so single episodes are turned down. */
  in_episodes?: boolean;
  /** The same work as the series source lists it. */
  as_series?: { tvmaze_id: number; title: string; episodes: number; minutes: number } | null;
  /** The poster at the size grids and lists show it. */
  poster_small?: string | null;
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
  /** How many compact versions the title has, and how big they are together. */
  compact_count?: number;
  compact_bytes?: number;
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
  /** The folder the file is in when it is a copy kept on another location, and that location. */
  base?: string | null;
  copy_on?: { name: string; online: boolean } | null;
  size: number;
  quality: Quality;
  release_group?: string | null;
  edition: string;
  media_info?: { video_codec: string; audio_codec: string; audio_channels: number; dynamic_range_type: string; width: number; height: number; runtime_secs: number } | null;
  scene_name?: string | null;
  added_at: number;
  extra?: boolean;
  /** The file this is a compact version of: a smaller copy Spool made of it. */
  compact_of?: number | null;
  /** A compact version of this file is being made, or waiting to be. */
  compacting?: boolean;
  /** For a film's own file: whether a compact version could be made of it, and how big. */
  compact?: { can: boolean; bytes?: number; why?: string };
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
