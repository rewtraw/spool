<script lang="ts">
  import { api, type Kind, type Title } from '../lib/api';
  import { app, act, go, loadTitles } from '../lib/state.svelte';
  import Poster from '../components/Poster.svelte';

  let kind = $state<Kind>('movie');
  let query = $state('');
  let results = $state<Title[]>([]);
  let searching = $state(false);
  let searched = $state(false);
  let error = $state('');
  let adding = $state<Title | null>(null);
  let profileId = $state(0);
  let monitored = $state(true);
  let searchNow = $state(true);
  let saving = $state(false);

  const profiles = $derived(app.profiles.filter((p) => p.kind === (kind === 'movie' ? 'movie' : 'tv')));

  let timer: ReturnType<typeof setTimeout>;
  let seq = 0;
  function onInput() {
    clearTimeout(timer);
    timer = setTimeout(run, 350);
  }

  async function run() {
    const q = query.trim();
    const mine = ++seq;
    if (!q) {
      results = [];
      searched = false;
      error = '';
      return;
    }
    searching = true;
    error = '';
    try {
      const r = await api.get<Title[]>(`/lookup?kind=${kind}&q=${encodeURIComponent(q)}`);
      if (mine !== seq) return;
      results = r;
      searched = true;
    } catch (e: any) {
      if (mine !== seq) return;
      error = e.message;
      results = [];
    } finally {
      if (mine === seq) searching = false;
    }
  }

  function open(t: Title) {
    adding = t;
    const preferred = kind === 'movie' ? app.status?.default_movie_profile : app.status?.default_series_profile;
    profileId = profiles.find((p) => p.id === preferred)?.id ?? profiles[0]?.id ?? 0;
    monitored = true;
    searchNow = app.status?.mode === 'active';
  }

  async function add() {
    if (!adding) return;
    saving = true;
    const t = await act(() => api.post<Title>('/titles', { kind, tmdb_id: adding!.tmdb_id, tvmaze_id: adding!.tvmaze_id, profile_id: profileId, monitored, search: searchNow && monitored }), `Added ${adding.title}`);
    saving = false;
    if (t) {
      adding = null;
      await loadTitles();
      go(`title/${t.id}`);
    }
  }
</script>

<header class="head"><h1>Discover</h1></header>

<div class="bar">
  <div class="seg">
    <button class:on={kind === 'movie'} onclick={() => { kind = 'movie'; run(); }}>Movies</button>
    <button class:on={kind === 'series'} onclick={() => { kind = 'series'; run(); }}>Series</button>
  </div>
  <input class="input grow" type="search" placeholder={kind === 'movie' ? 'Search movies by title, or paste an IMDb id' : 'Search series by title'} bind:value={query} oninput={onInput} onkeydown={(e) => e.key === 'Enter' && run()} data-search />
</div>

{#if error}
  <div class="card empty">{error}{#if error.includes('TMDB')} <a class="link" href="#/settings">Open Settings</a>{/if}</div>
{:else if searching && !results.length}
  <div class="empty">Searching…</div>
{:else if searched && !results.length}
  <div class="card empty">No {kind === 'movie' ? 'movies' : 'series'} found for “{query.trim()}”.</div>
{:else if !searched}
  <div class="card empty">Find a {kind === 'movie' ? 'movie' : 'series'} to add to the library.</div>
{/if}

{#if results.length}<p class="faint source">{kind === 'movie' ? 'Results from TMDB.' : 'Results from TVmaze.'}</p>{/if}
<div class="results">
  {#each results as r (r.tmdb_id ?? r.tvmaze_id)}
    <div class="card result">
      <div class="art"><Poster src={r.poster} title={r.title} kind={r.kind} /></div>
      <div class="grow body">
        <div class="row wrap"><h2>{r.title}</h2><span class="muted">{r.year || ''}</span></div>
        <div class="faint meta">{[r.network ?? r.studio, r.status === 'ended' ? 'Ended' : r.status === 'continuing' ? 'Continuing' : '', r.runtime ? `${r.runtime} min` : '', r.genres.slice(0, 3).join(', ')].filter(Boolean).join(' · ')}</div>
        <p class="overview">{r.overview}</p>
        <div class="row">
          {#if r.library_id}
            <a class="btn small" href="#/title/{r.library_id}">In library</a>
          {:else}
            <button class="btn small primary" onclick={() => open(r)}>Add</button>
          {/if}
        </div>
      </div>
    </div>
  {/each}
</div>

{#if adding}
  <div class="scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && (adding = null)} onkeydown={(e) => e.key === 'Escape' && (adding = null)}>
    <div class="card dialog" role="dialog" aria-modal="true" aria-label="Add {adding.title}">
      <h2>Add {adding.title} {adding.year ? `(${adding.year})` : ''}</h2>
      <label class="field">Quality profile
        <select class="input" bind:value={profileId}>
          {#each profiles as p}<option value={p.id}>{p.name}</option>{/each}
        </select>
      </label>
      <label class="check"><input type="checkbox" bind:checked={monitored} /> Monitor, and get it when an acceptable release appears</label>
      <label class="check"><input type="checkbox" bind:checked={searchNow} disabled={!monitored || app.status?.mode !== 'active'} /> Search for it right away</label>
      {#if app.status?.mode !== 'active'}<p class="faint small">Spool is in shadow mode, so nothing will be downloaded.</p>{/if}
      {#if !profiles.length}<p class="faint small">There is no quality profile for {kind === 'movie' ? 'movies' : 'series'} yet. Create one in Settings first.</p>{/if}
      <div class="row end">
        <button class="btn ghost" onclick={() => (adding = null)}>Cancel</button>
        <button class="btn primary" disabled={saving || !profiles.length} onclick={add}>{saving ? 'Adding…' : 'Add to library'}</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .head {
    margin-bottom: 18px;
  }
  .bar {
    display: flex;
    gap: 10px;
    margin-bottom: 20px;
    flex-wrap: wrap;
  }
  .bar .input {
    flex: 1 1 240px;
    width: auto;
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
    padding: 0 12px;
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
  }
  .source {
    font-size: 12.5px;
    margin-bottom: 10px;
  }
  .results {
    display: grid;
    gap: 12px;
  }
  .result {
    display: flex;
    gap: 16px;
    padding: 14px;
  }
  .art {
    width: 92px;
    flex: none;
  }
  .body {
    display: grid;
    gap: 6px;
    align-content: start;
  }
  .meta {
    font-size: 13px;
  }
  .overview {
    color: var(--muted);
    font-size: 14px;
    display: -webkit-box;
    -webkit-line-clamp: 3;
    line-clamp: 3;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .link {
    color: var(--accent);
    font-weight: 550;
  }
  .scrim {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.55);
    display: grid;
    place-items: center;
    padding: 18px;
    z-index: 40;
  }
  .dialog {
    width: min(460px, 100%);
    padding: 22px;
    display: grid;
    gap: 16px;
    box-shadow: var(--shadow);
  }
  .end {
    justify-content: flex-end;
  }
  .small {
    font-size: 13px;
  }
  @media (max-width: 760px) {
    .art {
      width: 70px;
    }
  }
</style>
