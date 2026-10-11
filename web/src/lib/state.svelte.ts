// Shared reactive state: router, live events, toasts, and the data every view needs.
import { api, type Collection, type Job, type Title } from './api';
import { setQualities } from './format';

function parseRoute() {
  const hash = location.hash.replace(/^#\/?/, '');
  const [path, query] = hash.split('?');
  const parts = path.split('/').filter(Boolean);
  return { view: parts[0] || 'home', arg: parts[1] ?? '', sub: parts[2] ?? '', query: new URLSearchParams(query ?? '') };
}

export const route = $state(parseRoute());
window.addEventListener('hashchange', () => Object.assign(route, parseRoute()));

export function go(path: string) {
  location.hash = '#/' + path.replace(/^\//, '');
}

export const app = $state({
  status: null as any,
  titles: [] as Title[],
  collections: [] as Collection[],
  titlesLoaded: false,
  profiles: [] as any[],
  jobs: {} as Record<string, Job>,
  attention: 0,
  /** Bumped when something changes that open views should refetch. */
  tick: 0,
  titleTick: {} as Record<number, number>,
  online: true,
  signedOut: false,
  toasts: [] as { id: number; text: string; kind: 'info' | 'error' }[],
});

let toastId = 0;
export function toast(text: string, kind: 'info' | 'error' = 'info') {
  const id = ++toastId;
  app.toasts.push({ id, text, kind });
  setTimeout(() => {
    const i = app.toasts.findIndex((t) => t.id === id);
    if (i >= 0) app.toasts.splice(i, 1);
  }, kind === 'error' ? 7000 : 3500);
}

/** Run an action and report failure in a toast. Returns undefined on failure. */
export async function act<T>(fn: () => Promise<T>, success?: string): Promise<T | undefined> {
  try {
    const r = await fn();
    if (success) toast(success);
    return r;
  } catch (e: any) {
    toast(e?.message ?? 'Something went wrong', 'error');
    return undefined;
  }
}

export async function loadStatus() {
  try {
    app.status = await api.get('/status');
    app.attention = app.status.attention;
    app.online = true;
  } catch (e: any) {
    if (e?.status === 0) app.online = false;
  }
}

type Listing = { revision: number; full: boolean; titles: Title[]; removed: number[] };
/** The library's change count as of the list held here; zero until there is one. */
let revision = 0;
let listedAt = 0;
let loading: Promise<void> = Promise.resolve();

async function fetchTitles(full: boolean) {
  try {
    // Whether a film is out yet turns on the date, which no change marks, so now and then
    // the whole list is read again.
    const whole = full || Date.now() - listedAt > 3600_000;
    const r = await api.get<Listing>(`/titles?since=${whole ? 0 : revision}`);
    if (r.full) {
      app.titles = r.titles;
      listedAt = Date.now();
    } else if (r.titles.length || r.removed.length) {
      const gone = new Set(r.removed);
      const fresh = new Map(r.titles.map((t) => [t.id, t]));
      const next = app.titles.filter((t) => !gone.has(t.id)).map((t) => fresh.get(t.id) ?? t);
      const known = new Set(next.map((t) => t.id));
      for (const t of r.titles) if (!known.has(t.id)) next.push(t);
      app.titles = next.sort((a, b) => a.id - b.id);
    }
    // Which titles belong together follows from the titles, so it is read with them.
    if (r.full || r.titles.length || r.removed.length || !app.titlesLoaded) {
      api.get<Collection[]>('/collections').then((c) => (app.collections = c)).catch(() => {});
    }
    revision = r.revision;
    app.titlesLoaded = true;
  } catch {
    /* handled by status */
  }
}

/** Bring the title list up to date: only what changed, unless `full` asks for all of it. */
export function loadTitles(full = false) {
  // One at a time, so an older answer never lands on top of a newer one.
  loading = loading.then(() => fetchTitles(full));
  return loading;
}

export async function loadProfiles() {
  try {
    app.profiles = await api.get('/profiles');
  } catch {
    /* ignore */
  }
}

let titlesTimer: ReturnType<typeof setTimeout> | undefined;
function titlesSoon() {
  clearTimeout(titlesTimer);
  titlesTimer = setTimeout(() => {
    loadTitles();
    loadStatus();
  }, 400);
}

let source: EventSource | undefined;
export function connect() {
  source?.close();
  source = new EventSource('/api/events');
  source.onopen = () => {
    // Anything missed while the connection was down arrives now.
    if (!app.online && app.titlesLoaded) loadTitles();
    app.online = true;
  };
  source.onerror = () => {
    // The browser reconnects by itself; state is refreshed when it does.
    app.online = false;
  };
  source.onmessage = (m) => {
    app.online = true;
    let e: any;
    try {
      e = JSON.parse(m.data);
    } catch {
      return;
    }
    switch (e.type) {
      case 'job':
        app.jobs[e.job.id] = e.job;
        break;
      case 'acquisition':
        app.tick++;
        app.titleTick[e.title_id] = (app.titleTick[e.title_id] ?? 0) + 1;
        titlesSoon();
        break;
      case 'title':
        app.titleTick[e.id] = (app.titleTick[e.id] ?? 0) + 1;
        titlesSoon();
        break;
      case 'decisions':
        app.titleTick[e.title_id] = (app.titleTick[e.title_id] ?? 0) + 1;
        break;
      case 'attention':
      case 'task':
        app.tick++;
        loadStatus();
        break;
      case 'locations':
        // Titles go offline or come back without anything about them changing.
        app.tick++;
        loadTitles(true);
        loadStatus();
        break;
      case 'resync':
        app.tick++;
        loadTitles(true);
        loadStatus();
        break;
    }
  };
}

export async function boot() {
  const session = await api.get('/session').catch(() => ({ required: false, signed_in: true }));
  if (session.required && !session.signed_in) {
    app.signedOut = true;
    return;
  }
  app.signedOut = false;
  api.get('/qualities').then(setQualities).catch(() => {});
  await Promise.all([loadStatus(), loadTitles(true), loadProfiles()]);
  connect();
}

window.addEventListener('spool:signin', () => {
  app.signedOut = true;
});

// Safety net for missed events and for tabs that were asleep.
setInterval(() => {
  if (!document.hidden && !app.signedOut) loadStatus();
}, 30000);
document.addEventListener('visibilitychange', () => {
  if (!document.hidden && !app.signedOut) {
    loadStatus();
    loadTitles(true);
    app.tick++;
  }
});
