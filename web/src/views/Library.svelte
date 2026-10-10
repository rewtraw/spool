<script lang="ts">
  import { app, go, act, toast, loadTitles } from '../lib/state.svelte';
  import { api } from '../lib/api';
  import type { Title } from '../lib/api';
  import { bytes } from '../lib/format';
  import Poster from '../components/Poster.svelte';

  const saved = JSON.parse(localStorage.getItem('spool.library') ?? '{}');
  let kind = $state<'all' | 'movie' | 'series'>(saved.kind ?? 'all');
  let show = $state<'all' | 'have' | 'missing' | 'monitored' | 'unmonitored'>(saved.show ?? 'all');
  let sort = $state<'title' | 'added' | 'year' | 'size'>(saved.sort ?? 'title');
  let layout = $state<'grid' | 'table'>(saved.layout ?? 'grid');
  let query = $state('');
  let limit = $state(120);

  $effect(() => {
    localStorage.setItem('spool.library', JSON.stringify({ kind, show, sort, layout }));
  });

  function have(t: Title) {
    return t.kind === 'movie' ? t.file_count > 0 : t.episodes_aired > 0 && t.episodes_have >= t.episodes_aired;
  }

  function statusOf(t: Title): { label: string; tone: string } {
    if (t.active === 'import_blocked') return { label: 'Needs attention', tone: 'warn' };
    if (t.active) return { label: 'Downloading', tone: 'accent' };
    if (t.kind === 'series') {
      if (t.episodes_aired === 0) return { label: t.episodes_have ? `${t.episodes_have} eps` : 'Not aired', tone: '' };
      if (t.episodes_have >= t.episodes_aired) return { label: `${t.episodes_have} / ${t.episodes_aired}`, tone: 'ok' };
      return { label: `${t.episodes_have} / ${t.episodes_aired}`, tone: t.monitored ? 'warn' : '' };
    }
    if (t.file_count > 0) return { label: 'In library', tone: 'ok' };
    if (!t.monitored) return { label: 'Not monitored', tone: '' };
    if (!t.available) return { label: 'Not released', tone: '' };
    return { label: 'Missing', tone: 'warn' };
  }

  const filtered = $derived.by(() => {
    const q = query.trim().toLowerCase();
    let list = app.titles.filter((t) => {
      if (kind !== 'all' && t.kind !== kind) return false;
      if (show === 'have' && !(t.kind === 'movie' ? t.file_count > 0 : t.episodes_have > 0)) return false;
      if (show === 'missing' && (have(t) || !t.monitored || !t.available)) return false;
      if (show === 'monitored' && !t.monitored) return false;
      if (show === 'unmonitored' && t.monitored) return false;
      if (q && !t.title.toLowerCase().includes(q) && String(t.year) !== q) return false;
      return true;
    });
    const by: Record<string, (a: Title, b: Title) => number> = {
      title: (a, b) => a.sort_title.localeCompare(b.sort_title),
      added: (a, b) => b.added_at - a.added_at,
      year: (a, b) => b.year - a.year || a.sort_title.localeCompare(b.sort_title),
      size: (a, b) => b.size - a.size,
    };
    return list.sort(by[sort]);
  });
  const shown = $derived(filtered.slice(0, limit));

  $effect(() => {
    // Reset paging when the filter changes.
    kind; show; sort; query;
    limit = 120;
  });

  let sentinel = $state<HTMLElement>();
  $effect(() => {
    if (!sentinel) return;
    const io = new IntersectionObserver((entries) => {
      if (entries.some((e) => e.isIntersecting)) limit += 120;
    }, { rootMargin: '600px' });
    io.observe(sentinel);
    return () => io.disconnect();
  });

  // ---- acting on several titles at once
  let selecting = $state(false);
  let picked = $state<Record<number, boolean>>({});
  let lastPicked = -1;
  const chosen = $derived(app.titles.filter((t) => picked[t.id]));
  const chosenBytes = $derived(chosen.reduce((s, t) => s + (t.size || 0), 0));
  let bulkProfile = $state('');

  function stopSelecting() {
    selecting = false;
    picked = {};
    lastPicked = -1;
  }
  // A click ticks one; a shift-click ticks everything between it and the last one ticked.
  function pick(e: MouseEvent | KeyboardEvent, t: Title) {
    e.preventDefault();
    const at = filtered.findIndex((x) => x.id === t.id);
    const on = !picked[t.id];
    if (e.shiftKey && lastPicked >= 0 && at >= 0) {
      const [a, b] = at < lastPicked ? [at, lastPicked] : [lastPicked, at];
      for (const x of filtered.slice(a, b + 1)) picked[x.id] = on;
    } else {
      picked[t.id] = on;
    }
    lastPicked = at;
  }
  function pickAll() {
    const all = filtered.every((t) => picked[t.id]);
    for (const t of filtered) picked[t.id] = !all;
  }
  const many = (n: number) => `${n} ${n === 1 ? 'title' : 'titles'}`;
  async function bulk(action: string, extra: Record<string, unknown> = {}, done?: (r: any) => string) {
    const ids = chosen.map((t) => t.id);
    if (!ids.length) return;
    const r = await act(() => api.post<any>('/titles/bulk', { ids, action, ...extra }));
    if (!r) return;
    toast(done ? done(r) : `Done for ${many(r.done ?? ids.length)}`);
    await loadTitles();
    if (action === 'remove' || action === 'free') stopSelecting();
  }
  function bulkFree() {
    const withFiles = chosen.filter((t) => t.size > 0).length;
    if (!withFiles) return toast('None of these have files on disk');
    if (!confirm(`Delete the files of ${many(withFiles)} (${bytes(chosenBytes)})? They stay in the library, unmonitored, so Spool does not fetch them again.`)) return;
    bulk('free', {}, (r) => `Deleted the files of ${many(r.done)}, ${bytes(r.bytes)}`);
  }
  function bulkRemove(deleteFiles: boolean) {
    const n = chosen.length;
    if (!confirm(deleteFiles ? `Remove ${many(n)} from the library and delete their files (${bytes(chosenBytes)})?` : `Remove ${many(n)} from the library? Their files stay on disk.`)) return;
    bulk('remove', { delete_files: deleteFiles }, (r) => `Removed ${many(r.done)}`);
  }
  function bulkSetProfile() {
    if (!bulkProfile) return;
    bulk('profile', { profile_id: Number(bulkProfile) }, (r) => (r.done ? `Profile changed for ${many(r.done)}` : 'That profile does not apply to any of the selected titles'));
    bulkProfile = '';
  }

  const counts = $derived({
    all: app.titles.length,
    movie: app.titles.filter((t) => t.kind === 'movie').length,
    series: app.titles.filter((t) => t.kind === 'series').length,
  });
</script>

<header class="head">
  <h1>Library</h1>
  <div class="row">
    <button class="btn" onclick={() => (selecting ? stopSelecting() : (selecting = true))} aria-pressed={selecting}>{selecting ? 'Done' : 'Select'}</button>
    <button class="btn primary" onclick={() => go('discover')}>Add</button>
  </div>
</header>

<div class="bar">
  <div class="seg" role="tablist" aria-label="Type">
    {#each [['all', 'All'], ['movie', 'Movies'], ['series', 'Series']] as [k, label]}
      <button role="tab" aria-selected={kind === k} class:on={kind === k} onclick={() => (kind = k as any)}>{label} <small>{counts[k as 'all']}</small></button>
    {/each}
  </div>
  <input class="input search" type="search" placeholder="Filter by title or year" bind:value={query} data-search aria-label="Filter library" />
  <select class="input narrow" bind:value={show} aria-label="Show">
    <option value="all">Everything</option>
    <option value="have">Have files</option>
    <option value="missing">Missing</option>
    <option value="monitored">Monitored</option>
    <option value="unmonitored">Not monitored</option>
  </select>
  <select class="input narrow" bind:value={sort} aria-label="Sort by">
    <option value="title">Title</option>
    <option value="added">Recently added</option>
    <option value="year">Year</option>
    <option value="size">Size</option>
  </select>
  <div class="seg" aria-label="Layout">
    <button class:on={layout === 'grid'} onclick={() => (layout = 'grid')} aria-pressed={layout === 'grid'}>Grid</button>
    <button class:on={layout === 'table'} onclick={() => (layout = 'table')} aria-pressed={layout === 'table'}>Table</button>
  </div>
</div>

{#if !app.titlesLoaded}
  <div class="empty">Loading…</div>
{:else if !filtered.length}
  <div class="card empty">{app.titles.length ? 'Nothing matches these filters.' : 'The library is empty. Add a title to get started.'}</div>
{:else if layout === 'grid'}
  <div class="grid">
    {#each shown as t (t.id)}
      {@const s = statusOf(t)}
      <a class="tile" href="#/title/{t.id}" class:dim={!t.monitored && !(t.file_count || t.episodes_have)} class:picked={picked[t.id]} onclick={(e) => selecting && pick(e, t)}>
        <div class="art">
          <Poster src={t.poster} title={t.title} kind={t.kind} />
          {#if selecting}<span class="check" class:on={picked[t.id]} aria-hidden="true">{picked[t.id] ? '✓' : ''}</span>{/if}
          <span class="chip {s.tone} pin">{s.label}</span>
        </div>
        <div class="name truncate">{t.title}</div>
        <div class="faint small">{t.year || ''}{t.kind === 'series' ? ' · Series' : ''}</div>
      </a>
    {/each}
  </div>
{:else}
  <div class="card scroll-x">
    <table class="table">
      <thead><tr>{#if selecting}<th style="width:1%"><input class="tick" type="checkbox" aria-label="Select all" checked={filtered.length > 0 && filtered.every((t) => picked[t.id])} onchange={pickAll} /></th>{/if}<th>Title</th><th>Year</th><th>Type</th><th>Status</th><th>Profile</th><th>Size</th></tr></thead>
      <tbody>
        {#each shown as t (t.id)}
          {@const s = statusOf(t)}
          <tr onclick={(e) => (selecting ? pick(e, t) : go(`title/${t.id}`))} class:picked={picked[t.id]}>
            {#if selecting}<td><input class="tick" type="checkbox" aria-label="Select {t.title}" checked={!!picked[t.id]} /></td>{/if}
            <td><a href="#/title/{t.id}"><b>{t.title}</b></a></td>
            <td class="muted">{t.year || ''}</td>
            <td class="muted">{t.kind === 'movie' ? 'Movie' : 'Series'}</td>
            <td><span class="chip {s.tone}">{s.label}</span>{#if !t.monitored}<span class="faint small"> not monitored</span>{/if}</td>
            <td class="muted">{t.profile ?? ''}</td>
            <td class="muted" style="white-space:nowrap">{t.size ? bytes(t.size) : ''}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
{/if}
<div bind:this={sentinel}></div>
{#if filtered.length}
  <p class="faint count">{filtered.length} {filtered.length === 1 ? 'title' : 'titles'}</p>
{/if}
{#if selecting}
  <div class="bulkbar" role="toolbar" aria-label="Actions for selected titles">
    <span class="n">{chosen.length} selected{chosenBytes ? ` · ${bytes(chosenBytes)}` : ''}</span>
    <button class="btn small ghost" onclick={pickAll}>{filtered.every((t) => picked[t.id]) ? 'Select none' : `Select all ${filtered.length}`}</button>
    <span class="sep"></span>
    {#if chosen.length}
      <button class="btn small" onclick={() => bulk('monitor', {}, (r) => `Monitoring ${many(r.done)}`)}>Monitor</button>
      <button class="btn small" onclick={() => bulk('unmonitor', {}, (r) => `Stopped monitoring ${many(r.done)}`)}>Stop monitoring</button>
      <select class="input small-select" bind:value={bulkProfile} onchange={bulkSetProfile} aria-label="Change profile">
        <option value="">Change profile…</option>
        {#each app.profiles as p}<option value={p.id}>{p.name} ({p.kind === 'movie' ? 'movies' : 'series'})</option>{/each}
      </select>
      <button class="btn small" onclick={() => bulk('search', {}, (r) => `Searching ${many(r.started)} in the background. Downloads appear in Activity.`)}>Search</button>
      <button class="btn small" onclick={() => bulk('refresh', {}, (r) => `Refreshing ${many(r.started)} in the background`)}>Refresh</button>
      <button class="btn small ghost danger" onclick={bulkFree}>Delete files</button>
      <button class="btn small ghost danger" onclick={() => bulkRemove(false)}>Remove</button>
      <button class="btn small ghost danger" onclick={() => bulkRemove(true)}>Remove and delete files</button>
    {:else}
      <span class="faint">Click titles to select them. Shift-click selects a run.</span>
    {/if}
  </div>
{/if}

<style>
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 18px;
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
    white-space: nowrap;
  }
  .seg button.on {
    background: var(--surface);
    color: var(--text);
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.18);
  }
  .seg small {
    color: var(--faint);
    font-weight: 400;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(138px, 1fr));
    gap: 22px 16px;
  }
  .tile {
    display: grid;
    gap: 4px;
    min-width: 0;
  }
  .tile.picked .art {
    outline: 3px solid var(--accent);
    outline-offset: 2px;
    border-radius: 10px;
  }
  .check {
    position: absolute;
    top: 7px;
    right: 7px;
    width: 24px;
    height: 24px;
    display: grid;
    place-items: center;
    border-radius: 50%;
    border: 2px solid #fff;
    background: rgba(0, 0, 0, 0.45);
    color: #fff;
    font-size: 14px;
    font-weight: 700;
  }
  .check.on {
    background: var(--accent);
    border-color: var(--accent);
  }
  tr.picked {
    background: var(--accent-soft);
  }
  .small-select {
    width: auto;
    height: 30px;
    padding: 0 8px;
    font-size: 13.5px;
  }
  .tile.dim .art {
    opacity: 0.55;
  }
  .art {
    position: relative;
    transition: transform 0.15s ease;
  }
  .tile:hover .art,
  .tile:focus-visible .art {
    transform: translateY(-3px);
  }
  .pin {
    position: absolute;
    left: 7px;
    bottom: 7px;
    backdrop-filter: blur(8px);
    background: color-mix(in srgb, var(--bg) 78%, transparent);
  }
  .name {
    font-weight: 600;
    font-size: 14px;
    margin-top: 4px;
  }
  .small {
    font-size: 12.5px;
  }
  tbody tr {
    cursor: pointer;
  }
  tbody tr:hover {
    background: var(--raised);
  }
  .count {
    margin-top: 18px;
    font-size: 13px;
  }
  @media (max-width: 760px) {
    .grid {
      grid-template-columns: repeat(auto-fill, minmax(104px, 1fr));
      gap: 16px 10px;
    }
  }
</style>
