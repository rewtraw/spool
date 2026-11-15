<script lang="ts">
  // Videos from YouTube, X, Instagram and other sites, by their links. Each is filed under the
  // channel or account that posted it, which the library keeps as a series of its videos.
  import { onDestroy, untrack } from 'svelte';
  import { api } from '../lib/api';
  import { act, toast } from '../lib/state.svelte';
  import { ago, bytes, duration, pct, speed } from '../lib/format';
  import Poster from './Poster.svelte';

  let { initial = '' }: { initial?: string } = $props();

  type Progress = { done_bytes: number; total_bytes: number; speed: number; eta: number | null; part: number };
  type Video = { id: number; url: string; state: 'queued' | 'fetching' | 'filing' | 'done' | 'failed'; site: string; site_name: string; title: string; channel: string; day?: string | null; duration?: number | null; title_id?: number | null; error: string; added_at: number; updated_at: number; progress: Progress | null };
  type Channel = { title_id: number; title: string; site: string; site_name: string; url: string; follow: boolean; checked_at: number; videos: number; poster?: string | null };
  type Ready = { yt_dlp: boolean; folder: string | null; cookies: string };

  // The link it was opened with; it is opened again for another.
  let url = $state(untrack(() => initial));
  let take = $state<'newest' | 'all' | 'new'>('newest');
  let follow = $state(false);
  let busy = $state(false);
  let videos = $state<Video[]>([]);
  let channels = $state<Channel[]>([]);
  let ready = $state<Ready | null>(null);
  let cookies = $state('');
  let showSignIn = $state(false);

  const going = $derived(videos.filter((v) => v.state === 'queued' || v.state === 'fetching' || v.state === 'filing'));
  const finished = $derived(videos.filter((v) => v.state === 'done' || v.state === 'failed'));

  async function load() {
    try {
      const r = await api.get<{ videos: Video[]; channels: Channel[]; ready: Ready }>('/videos');
      videos = r.videos;
      channels = r.channels.sort((a, b) => a.title.localeCompare(b.title));
      if (!ready) cookies = r.ready.cookies;
      ready = r.ready;
    } catch {}
  }

  // Quickly while something is being fetched, now and then otherwise.
  let timer: ReturnType<typeof setTimeout> | null = null;
  async function tick() {
    await load();
    timer = setTimeout(tick, going.length ? 2000 : 15000);
  }
  tick();
  onDestroy(() => timer && clearTimeout(timer));

  async function get() {
    const link = url.trim();
    if (!link) return;
    busy = true;
    try {
      const r = await api.post<{ added: number; already: number; what: string; name: string; listed: number; title_id?: number | null }>('/videos', { url: link, take, follow });
      if (r.what === 'video') {
        toast(r.added ? `Fetching “${r.name}”` : `“${r.name}” is here or on its way already`);
      } else {
        const got = take === 'new' ? 'Nothing fetched now' : `${r.added} ${r.added === 1 ? 'video' : 'videos'} on the way${r.already ? `, ${r.already} here already` : ''}`;
        toast(`${r.name}: ${got}${follow ? '. Following it.' : '.'}`);
      }
      url = '';
      await load();
    } catch (e: any) {
      toast(e.message, 'error');
    } finally {
      busy = false;
    }
  }

  async function followChannel(c: Channel, on: boolean) {
    await act(() => api.post(`/channels/${c.title_id}/follow`, { follow: on }), on ? `Following ${c.title}` : `No longer following ${c.title}`);
    load();
  }

  async function saveCookies() {
    await act(() => api.patch('/settings', { video_cookies: cookies.trim() }), 'Saved');
    load();
  }

  function line(v: Video): string {
    if (v.state === 'queued') return 'Waiting its turn';
    if (v.state === 'filing') return 'Filing into the library';
    if (v.state === 'failed') return v.error || 'Failed';
    if (v.state === 'done') return `Filed ${ago(v.updated_at)}`;
    const p = v.progress;
    if (!p || !p.total_bytes) return 'Starting';
    const parts = [`${p.part > 1 ? 'Sound: ' : ''}${bytes(p.done_bytes)} of ${bytes(p.total_bytes)}`];
    if (p.speed) parts.push(speed(p.speed));
    if (p.eta != null) parts.push(`${duration(p.eta)} left`);
    return parts.join(' · ');
  }
</script>

{#if ready && !ready.yt_dlp}
  <div class="card notice warn">yt-dlp is not installed on the machine Spool runs on, and it does the fetching. Install it there with <span class="mono">brew install yt-dlp</span>, then come back.</div>
{:else if ready && !ready.folder}
  <div class="card notice warn">There is no videos folder to file them in. Add one to a location in <a class="link" href="#/settings/storage">Settings → Storage</a>.</div>
{/if}

<form class="card get" onsubmit={(e) => { e.preventDefault(); get(); }}>
  <div class="row wrap">
    <input class="input grow mono" type="url" inputmode="url" placeholder="A link to a video, a channel, an account or a playlist" bind:value={url} aria-label="Link" data-search />
    <button class="btn primary" type="submit" disabled={busy || !url.trim()}>{busy ? 'Looking…' : 'Get'}</button>
  </div>
  <div class="row wrap opts">
    <label class="field inline">From a channel or playlist
      <select class="input auto" bind:value={take}>
        <option value="newest">its 10 newest videos</option>
        <option value="all">every video it has</option>
        <option value="new">nothing yet</option>
      </select>
    </label>
    <label class="check"><input type="checkbox" bind:checked={follow} /> Follow, and fetch what it posts from now on</label>
  </div>
  <p class="faint small">YouTube, X, Instagram, TikTok, Vimeo and well over a thousand other sites. Each video is filed under the channel or account that posted it, a year at a time, with the site's captions as subtitles, so it can be played, searched for what is said and clipped like any episode.</p>
</form>

{#if going.length}
  <div class="section-title">On the way</div>
  <div class="stack">
    {#each going as v (v.id)}
      <div class="card item">
        <div class="grow">
          <div class="row wrap top"><span class="t">{v.title || v.url}</span><span class="chip">{v.site_name}</span></div>
          {#if v.channel}<div class="faint small">{v.channel}{v.day ? ` · ${v.day}` : ''}{v.duration ? ` · ${duration(v.duration)}` : ''}</div>{/if}
          <div class="muted small">{line(v)}</div>
          {#if v.state === 'fetching' && v.progress?.total_bytes}<div class="progress" role="progressbar" aria-label="Fetched" aria-valuenow={Math.round(pct(v.progress.done_bytes, v.progress.total_bytes))} aria-valuemin="0" aria-valuemax="100"><i style="width:{pct(v.progress.done_bytes, v.progress.total_bytes)}%"></i></div>{/if}
        </div>
        <div class="buttons"><button class="btn small ghost danger" onclick={async () => { await act(() => api.del(`/videos/${v.id}`)); load(); }}>Remove</button></div>
      </div>
    {/each}
  </div>
{/if}

{#if channels.length}
  <div class="section-title">Channels</div>
  <div class="channels">
    {#each channels as c (c.title_id)}
      <div class="card channel">
        <a class="art" href="#/title/{c.title_id}"><Poster src={c.poster} title={c.title} kind="series" /></a>
        <div class="grow body">
          <a class="t" href="#/title/{c.title_id}">{c.title}</a>
          <div class="faint small">{c.site_name} · {c.videos} {c.videos === 1 ? 'video' : 'videos'}</div>
          <label class="check small"><input type="checkbox" checked={c.follow} onchange={(e) => followChannel(c, e.currentTarget.checked)} /> Follow</label>
        </div>
      </div>
    {/each}
  </div>
{/if}

{#if finished.length}
  <div class="row section-head">
    <div class="section-title">Recently</div>
    <button class="btn small ghost" onclick={async () => { await act(() => api.post('/videos/clear', {})); load(); }}>Clear</button>
  </div>
  <div class="card scroll-x">
    <table class="table">
      <tbody>
        {#each finished.slice(0, 50) as v (v.id)}
          <tr>
            <td style="white-space:nowrap"><span class="chip {v.state === 'done' ? 'ok' : 'bad'}">{v.state === 'done' ? 'Filed' : 'Failed'}</span></td>
            <td>
              {#if v.title_id}<a href="#/title/{v.title_id}"><b>{v.title || v.url}</b></a>{:else}<b class="break">{v.title || v.url}</b>{/if}
              <div class="faint small">{[v.channel, v.site_name].filter(Boolean).join(' · ')}</div>
              {#if v.state === 'failed'}<div class="err">{v.error}</div>{/if}
            </td>
            <td style="white-space:nowrap">
              {#if v.state === 'failed'}<button class="btn small" onclick={async () => { await act(() => api.post(`/videos/${v.id}/retry`, {})); load(); }}>Try again</button>{/if}
              <button class="btn small ghost" onclick={async () => { await act(() => api.del(`/videos/${v.id}`)); load(); }} aria-label="Forget">×</button>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
{/if}

<div class="signin">
  <button class="link small" aria-expanded={showSignIn} onclick={() => (showSignIn = !showSignIn)}>Signing in to sites{ready?.cookies ? ` (${ready.cookies.startsWith('/') ? 'a cookies file' : ready.cookies})` : ''}</button>
  {#if showSignIn}
    <div class="card pad stack">
      <p class="muted small">Instagram, and some posts on X, are only shown to someone signed in. yt-dlp can borrow the sign-in of a browser on the machine Spool runs on (sign in to the site there first), or read a cookies.txt file exported from a browser.</p>
      <label class="field">Browser, or the full path of a cookies.txt file
        <input class="input mono" list="browsers" placeholder="Nowhere" bind:value={cookies} />
        <datalist id="browsers"><option value="safari"></option><option value="chrome"></option><option value="firefox"></option><option value="brave"></option><option value="edge"></option></datalist>
      </label>
      <div><button class="btn small" onclick={saveCookies}>Save</button></div>
    </div>
  {/if}
</div>

<style>
  .get { padding: 14px; display: grid; gap: 10px; margin-bottom: 18px; }
  .opts { gap: 14px; align-items: center; }
  .field.inline { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
  .section-title { margin: 22px 0 10px; }
  .section-head { justify-content: space-between; align-items: center; }
  .item { display: flex; gap: 12px; padding: 12px 14px; align-items: flex-start; }
  .item .t { font-weight: 600; overflow-wrap: anywhere; }
  .top { gap: 8px; align-items: center; }
  .channels { display: grid; grid-template-columns: repeat(auto-fill, minmax(240px, 1fr)); gap: 10px; }
  .channel { display: flex; gap: 12px; padding: 10px; align-items: center; }
  .channel .art { width: 56px; flex: none; }
  .channel .body { display: grid; gap: 3px; min-width: 0; }
  .channel .t { font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .notice { padding: 12px 15px; border-radius: 9px; font-size: 14px; margin-bottom: 14px; background: var(--accent-soft); }
  .notice.warn { background: color-mix(in srgb, var(--warn) 15%, transparent); }
  .err { color: var(--bad); font-size: 0.9em; overflow-wrap: anywhere; }
  .signin { margin-top: 26px; }
  .break { overflow-wrap: anywhere; }
</style>
