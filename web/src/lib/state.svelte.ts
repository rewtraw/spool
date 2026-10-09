// Shared reactive state: router, live events, toasts, and the data every view needs.
import { api, type Job, type Title } from './api';
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

export async function loadTitles() {
  try {
    app.titles = await api.get<Title[]>('/titles');
    app.titlesLoaded = true;
  } catch {
    /* handled by status */
  }
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
      case 'resync':
        app.tick++;
        titlesSoon();
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
  await Promise.all([loadStatus(), loadTitles(), loadProfiles()]);
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
    loadTitles();
    app.tick++;
  }
});
