<script lang="ts">
  // An album wanted and not here yet: what Spool is doing about it, and what it has tried.
  import { api } from '../lib/api';
  import { act, go } from '../lib/state.svelte';
  import { ago, bytes } from '../lib/format';

  let { id }: { id: number } = $props();
  let w = $state<any>(null);
  let cover = $state('');
  let failed = $state(false);

  async function load() {
    const r = await api.get<{ wants: any[] }>('/music/wants').catch(() => null);
    const found = r?.wants.find((x) => x.want.id === id);
    if (!found) return void (failed = true);
    w = found.want;
    cover = found.cover;
  }
  load();
  $effect(() => {
    if (!w || w.state === 'have') return;
    const t = setInterval(load, 5000);
    return () => clearInterval(t);
  });

  async function again() {
    if (await act(() => api.post(`/music/wants/${id}/search`, {}), 'Looking for it')) load();
  }
  async function remove() {
    if (await act(() => api.del(`/music/wants/${id}`), w?.state === 'downloading' ? 'Stopped wanting it, and its download' : 'Stopped wanting it')) go('library?kind=music');
  }
  const LABEL: Record<string, { text: string; tone: string }> = { wanted: { text: 'Wanted', tone: 'warn' }, downloading: { text: 'Downloading', tone: 'accent' }, have: { text: 'Here', tone: 'ok' } };
</script>

<p class="back"><a class="link" href="#/library?kind=music">← Music</a></p>

{#if failed}
  <div class="card empty">This album is not wanted any more.</div>
{:else if !w}
  <div class="empty">Loading…</div>
{:else}
  {@const l = LABEL[w.state]}
  <div class="top">
    <div class="cover"><img src={cover} alt="" onerror={(e) => ((e.currentTarget as HTMLImageElement).style.visibility = 'hidden')} /></div>
    <div class="about">
      <div class="row wrap"><h1>{w.title}</h1><span class="chip {l?.tone}">{l?.text ?? w.state}</span></div>
      <div class="muted">{w.artist}{w.year ? ` · ${w.year}` : ''}{w.kind ? ` · ${w.kind}` : ''}</div>
      <div class="faint small">Wanted {ago(w.added_at)}{w.tracks ? ` · ${w.tracks[0] === w.tracks[1] ? w.tracks[0] : `${w.tracks[0]}–${w.tracks[1]}`} tracks` : ''}</div>
      {#if w.note}<div class="note">{w.note}</div>{/if}
      {#if w.release}<div class="faint small mono">{w.release.title} · {w.release.indexer}{w.release.size ? ` · ${bytes(w.release.size)}` : ''}</div>{/if}
      {#if w.state === 'have' && w.folder}<div class="faint small mono">{w.folder}</div>{/if}
      <div class="row wrap actions">
        {#if w.state === 'wanted'}<button class="btn small" onclick={again}>Look now</button>{/if}
        {#if w.state === 'downloading'}<a class="btn small" href="#/activity">Activity</a>{/if}
        <button class="btn small ghost danger" onclick={remove}>{w.state === 'have' ? 'Clear' : 'Stop wanting it'}</button>
      </div>
    </div>
  </div>
  {#if w.turned_down.length}
    <div class="section-title">Releases passed over</div>
    <div class="card list">
      {#each w.turned_down as t}<div><div class="mono small clamp">{t.name}</div><div class="faint small">{t.why} · {ago(t.at)}</div></div>{/each}
    </div>
  {/if}
{/if}

<style>
  .back {
    margin: 0 0 14px;
  }
  .top {
    display: flex;
    gap: 20px;
    align-items: flex-start;
    flex-wrap: wrap;
  }
  .cover {
    width: 200px;
    max-width: 42vw;
    aspect-ratio: 1;
    border-radius: 8px;
    overflow: hidden;
    background: var(--raised);
    border: 1px solid var(--line);
  }
  .cover img {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
  .about {
    display: grid;
    gap: 6px;
    flex: 1 1 280px;
    min-width: 0;
  }
  .about h1 {
    margin: 0;
  }
  .actions {
    margin-top: 8px;
    gap: 6px;
  }
  .section-title {
    margin: 24px 0 10px;
  }
  .list {
    display: grid;
    gap: 10px;
  }
  .clamp {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
