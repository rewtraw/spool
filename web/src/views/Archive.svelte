<script lang="ts">
  import { api } from '../lib/api';
  import { app, act, go } from '../lib/state.svelte';
  import { ago, bytes, qualityName } from '../lib/format';
  import Poster from '../components/Poster.svelte';

  type Saved = { id: number; release: { title: string; size: number; indexer: string }; quality: any; covers: string; source: string; fetched_at: number; checked_at: number; available: [number, number] | null; usable: boolean | null };
  type Group = { kind: 'movie' | 'series'; name: string; year: number; tmdb_id: number | null; tvdb_id: number | null; title_id: number | null; poster: string | null; releases: Saved[]; size: number; latest: number };

  let data = $state<{ groups: Group[]; count: number; stored_bytes: number } | null>(null);
  let query = $state('');
  let kind = $state<'all' | 'movie' | 'series'>('all');
  let show = $state<'all' | 'in' | 'out' | 'gone'>('all');
  let sort = $state<'title' | 'recent' | 'size'>('recent');
  let open = $state<Record<string, boolean>>({});
  let checking = $state<Record<string, boolean>>({});

  const SOURCE: Record<string, string> = { grabbed: 'Downloaded', runner_up: 'Fallback', manual: 'Added by hand' };
  const keyOf = (g: Group) => `${g.kind}:${g.tmdb_id ?? 0}:${g.tvdb_id ?? 0}`;
  const isActive = $derived(app.status?.mode === 'active');

  async function load() {
    data = await api.get('/archive').catch(() => data);
  }
  load();

  const filtered = $derived.by(() => {
    const q = query.trim().toLowerCase();
    let list = (data?.groups ?? []).filter((g) => {
      if (kind !== 'all' && g.kind !== kind) return false;
      if (show === 'in' && !g.title_id) return false;
      if (show === 'out' && g.title_id) return false;
      if (show === 'gone' && !g.releases.some((r) => r.usable === false)) return false;
      return !q || g.name.toLowerCase().includes(q) || g.releases.some((r) => r.release.title.toLowerCase().includes(q));
    });
    const by = { title: (a: Group, b: Group) => a.name.localeCompare(b.name), recent: (a: Group, b: Group) => b.latest - a.latest, size: (a: Group, b: Group) => b.size - a.size };
    return [...list].sort(by[sort]);
  });
  const outside = $derived((data?.groups ?? []).filter((g) => !g.title_id).length);
  const gone = $derived((data?.groups ?? []).flatMap((g) => g.releases).filter((r) => r.usable === false));

  function summary(g: Group) {
    const names = [...new Set(g.releases.map((r) => qualityName(r.quality, g.kind)))];
    return `${g.releases.length} ${g.releases.length === 1 ? 'release' : 'releases'} · ${names.slice(0, 3).join(', ')}${names.length > 3 ? '…' : ''}`;
  }
  function health(g: Group): { label: string; tone: string } | null {
    const checked = g.releases.filter((r) => r.usable !== null);
    if (!checked.length) return null;
    const ok = checked.filter((r) => r.usable).length;
    if (ok === checked.length) return { label: 'On Usenet', tone: 'ok' };
    if (ok === 0) return { label: 'Gone from Usenet', tone: 'bad' };
    return { label: `${ok} of ${checked.length} on Usenet`, tone: 'warn' };
  }

  async function check(r: Saved) {
    const res = await act(() => api.post<any>(`/archive/${r.id}/check`));
    if (res) Object.assign(r, res);
  }
  async function checkAll(g: Group) {
    const k = keyOf(g);
    checking[k] = true;
    for (const r of g.releases) {
      const res = await api.post<any>(`/archive/${r.id}/check`).catch(() => null);
      if (res) Object.assign(r, res);
    }
    checking[k] = false;
  }
  async function remove(r: Saved) {
    await act(() => api.del(`/archive/${r.id}`));
    load();
  }
  async function removeGone() {
    if (!confirm(`Remove ${gone.length} saved ${gone.length === 1 ? 'release' : 'releases'} that are no longer on Usenet?`)) return;
    await act(async () => {
      for (const r of gone) await api.del(`/archive/${r.id}`);
    }, 'Removed');
    load();
  }
  async function download(g: Group, r: Saved) {
    const res = await act(() => api.post<any>(`/archive/${r.id}/grab`), 'Download started');
    if (res && g.title_id) go(`title/${g.title_id}`);
  }
  async function restore(g: Group) {
    const t = await act(() => api.post<any>(`/archive/${g.releases[0].id}/restore`), `${g.name} is back in the library`);
    if (t) load();
  }
</script>

<div class="head">
  <h1>Archive</h1>
  {#if data}
    <div class="stats">
      <div><b>{data.groups.length}</b><span>Titles</span></div>
      <div><b>{data.count}</b><span>Releases</span></div>
      <div><b>{outside}</b><span>Not in library</span></div>
      <div><b>{bytes(data.stored_bytes)}</b><span>On disk</span></div>
    </div>
  {/if}
</div>
<p class="muted lede">Every release Spool has downloaded, and the next-best options it saw, kept so they can be fetched again without an indexer. Removing a title from the library leaves its releases here.</p>

<div class="bar">
  <input class="input search" data-search placeholder="Search titles and release names" bind:value={query} />
  <div class="seg" aria-label="Type">
    <button class:on={kind === 'all'} onclick={() => (kind = 'all')}>All</button>
    <button class:on={kind === 'movie'} onclick={() => (kind = 'movie')}>Films</button>
    <button class:on={kind === 'series'} onclick={() => (kind = 'series')}>Series</button>
  </div>
  <select class="input narrow" bind:value={show} aria-label="Show">
    <option value="all">Everything</option>
    <option value="in">In the library</option>
    <option value="out">Not in the library</option>
    <option value="gone">Gone from Usenet</option>
  </select>
  <select class="input narrow" bind:value={sort} aria-label="Sort by">
    <option value="recent">Recently saved</option>
    <option value="title">Title</option>
    <option value="size">Size</option>
  </select>
  {#if gone.length}<button class="btn" onclick={removeGone}>Remove {gone.length} gone</button>{/if}
</div>

{#if !data}
  <div class="empty">Loading…</div>
{:else if !data.groups.length}
  <div class="card empty">Nothing saved yet. Releases appear here from the next download on.</div>
{:else if !filtered.length}
  <div class="card empty">Nothing matches.</div>
{:else}
  <div class="stack">
    {#each filtered.slice(0, 200) as g (keyOf(g))}
      {@const k = keyOf(g)}
      {@const h = health(g)}
      <div class="card entry" class:open={open[k]}>
        <button class="summary" onclick={() => (open[k] = !open[k])} aria-expanded={!!open[k]}>
          <div class="thumb"><Poster src={g.poster} title={g.name} kind={g.kind} /></div>
          <div class="grow text">
            <div class="name">{g.name} <span class="muted year">{g.year || ''}</span></div>
            <div class="faint small">{g.kind === 'movie' ? 'Film' : 'Series'} · {summary(g)}</div>
            <div class="chips">
              <span class="chip {g.title_id ? '' : 'warn'}">{g.title_id ? 'In library' : 'Not in library'}</span>
              {#if h}<span class="chip {h.tone}">{h.label}</span>{/if}
            </div>
          </div>
          <div class="side">
            <div class="size">{bytes(g.size)}</div>
            <div class="faint small">saved {ago(g.latest)}</div>
          </div>
          <span class="caret" aria-hidden="true">›</span>
        </button>
        {#if open[k]}
          <div class="detail">
            <div class="row wrap actions">
              {#if g.title_id}<a class="btn small" href="#/title/{g.title_id}">Open title</a>
              {:else}<button class="btn small" onclick={() => restore(g)}>Add back to library</button>{/if}
              <button class="btn small ghost" disabled={checking[k]} onclick={() => checkAll(g)}>{checking[k] ? 'Checking…' : 'Check all on Usenet'}</button>
            </div>
            <div class="scroll-x">
              <table class="table">
                <thead><tr><th>Release</th><th>Quality</th><th>Size</th><th>Saved</th><th>On Usenet</th><th></th></tr></thead>
                <tbody>
                  {#each g.releases as r (r.id)}
                    <tr>
                      <td><div class="mono small break">{r.release.title}</div><div class="faint small">{SOURCE[r.source] ?? r.source}{r.release.indexer ? ` · from ${r.release.indexer}` : ''}{r.covers ? ` · ${r.covers}` : ''}</div></td>
                      <td style="white-space:nowrap">{qualityName(r.quality, g.kind)}</td>
                      <td class="muted" style="white-space:nowrap">{bytes(r.release.size)}</td>
                      <td class="muted" style="white-space:nowrap">{ago(r.fetched_at)}</td>
                      <td style="white-space:nowrap">
                        {#if r.usable == null}<span class="faint">Not checked</span>{:else if r.usable}<span class="chip ok">Available</span>{:else}<span class="chip bad">Gone</span>{/if}
                        {#if r.available}<div class="faint small">{r.available[0]} of {r.available[1]} sampled</div>{/if}
                      </td>
                      <td>
                        <div class="row tight">
                          {#if isActive && g.title_id}<button class="btn small" onclick={() => download(g, r)}>Download</button>{/if}
                          <button class="btn small ghost" onclick={() => check(r)}>Check</button>
                          <button class="btn small ghost" onclick={() => remove(r)}>Remove</button>
                        </div>
                      </td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            </div>
            {#if !g.title_id}<p class="faint small note">Add the title back to the library to download one of these.</p>{/if}
          </div>
        {/if}
      </div>
    {/each}
  </div>
  {#if filtered.length > 200}<p class="faint count">Showing 200 of {filtered.length}. Search to narrow the list.</p>{/if}
{/if}

<style>
  .head {
    display: flex;
    align-items: flex-end;
    justify-content: space-between;
    gap: 20px;
    flex-wrap: wrap;
    margin-bottom: 8px;
  }
  .stats {
    display: flex;
    gap: 28px;
  }
  .stats div {
    display: grid;
  }
  .stats b {
    font-family: var(--serif);
    font-size: 22px;
    font-weight: 600;
  }
  .stats span {
    color: var(--muted);
    font-size: 12.5px;
  }
  .lede {
    max-width: 70ch;
    margin: 0 0 20px;
  }
  .bar {
    display: flex;
    gap: 10px;
    flex-wrap: wrap;
    align-items: center;
    margin-bottom: 20px;
  }
  .search {
    flex: 1 1 220px;
    width: auto;
  }
  .narrow {
    width: auto;
    flex: 0 1 auto;
  }
  .seg {
    display: inline-flex;
    padding: 3px;
    border-radius: 9px;
    background: var(--raised);
    border: 1px solid var(--line);
  }
  .seg button {
    height: 28px;
    padding: 0 11px;
    border: none;
    background: transparent;
    border-radius: 6px;
    cursor: pointer;
    font-size: 13.5px;
    font-weight: 500;
    color: var(--muted);
  }
  .seg button.on {
    background: var(--surface);
    color: var(--text);
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.18);
  }
  .entry {
    padding: 0;
    overflow: hidden;
  }
  .summary {
    display: flex;
    align-items: center;
    gap: 16px;
    width: 100%;
    padding: 12px 16px;
    background: transparent;
    border: none;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  .summary:hover {
    background: var(--raised);
  }
  .thumb {
    width: 52px;
    flex: none;
  }
  .text {
    min-width: 0;
    display: grid;
    gap: 3px;
  }
  .name {
    font-family: var(--serif);
    font-size: 18px;
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .year {
    font-family: var(--sans);
    font-size: 14px;
    font-weight: 400;
  }
  .chips {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
    margin-top: 3px;
  }
  .side {
    text-align: right;
    white-space: nowrap;
    flex: none;
  }
  .size {
    font-weight: 600;
  }
  .caret {
    color: var(--faint);
    font-size: 20px;
    transition: transform 0.15s ease;
    flex: none;
  }
  .open .caret {
    transform: rotate(90deg);
  }
  .detail {
    border-top: 1px solid var(--line);
    padding: 14px 16px 16px;
    display: grid;
    gap: 12px;
  }
  .actions {
    gap: 8px;
  }
  .tight {
    gap: 6px;
    flex-wrap: nowrap;
  }
  .note,
  .count {
    margin: 0;
  }
  .count {
    margin-top: 16px;
  }
  @media (max-width: 640px) {
    .stats {
      gap: 18px;
    }
    .side {
      display: none;
    }
    .summary {
      gap: 12px;
      padding: 12px;
    }
  }
</style>
