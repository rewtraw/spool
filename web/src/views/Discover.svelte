<script lang="ts">
  import { api, type Kind, type Title } from '../lib/api';
  import { app, act, go, loadTitles } from '../lib/state.svelte';
  import Poster from '../components/Poster.svelte';
  import { bytes } from '../lib/format';

  let kind = $state<Kind>('movie');
  let query = $state('');
  type Found = Title & { library_id?: number | null; anime?: { format: string; episodes: number; name: string; as_series?: boolean } };
  let found = $state<{ movies: Found[]; series: Found[]; movies_error?: string | null }>({ movies: [], series: [] });
  // The kind asked for first, then what the other kind has under the same name.
  const results = $derived(kind === 'movie' ? found.movies : found.series);
  const others = $derived(kind === 'movie' ? found.series : found.movies);
  const FORMAT: Record<string, string> = { TV: 'series', TV_SHORT: 'short series', MOVIE: 'film', OVA: 'OVA', ONA: 'web series', SPECIAL: 'special' };
  let searching = $state(false);
  let searched = $state(false);
  let error = $state('');
  let adding = $state<Title | null>(null);
  let profileId = $state(0);
  let monitored = $state(true);
  let searchNow = $state(true);
  let saving = $state(false);
  // Where the new title is kept, when there is more than one place that can take it.
  let places = $state<any[]>([]);
  let placeId = $state(0);
  api.get<any[]>('/locations').then((p) => (places = p)).catch(() => {});
  const choices = $derived(places.filter((p) => p.manage && p.online && (addingKind === 'movie' ? p.movie_root : p.series_root)));

  const addingKind = $derived(adding?.kind ?? kind);
  const profiles = $derived(app.profiles.filter((p) => p.kind === (addingKind === 'movie' ? 'movie' : 'tv')));

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
      found = { movies: [], series: [] };
      searched = false;
      error = '';
      return;
    }
    searching = true;
    error = '';
    try {
      const r = await api.get<typeof found>(`/lookup/all?q=${encodeURIComponent(q)}`);
      if (mine !== seq) return;
      found = r;
      searched = true;
    } catch (e: any) {
      if (mine !== seq) return;
      error = e.message;
      found = { movies: [], series: [] };
    } finally {
      if (mine === seq) searching = false;
    }
  }

  function open(t: Title) {
    adding = t;
    const of = app.profiles.filter((p) => p.kind === (t.kind === 'movie' ? 'movie' : 'tv'));
    const preferred = t.kind === 'movie' ? app.status?.default_movie_profile : app.status?.default_series_profile;
    profileId = of.find((p) => p.id === preferred)?.id ?? of[0]?.id ?? 0;
    monitored = true;
    searchNow = app.status?.mode === 'active';
    placeId = places.find((p) => p.new_titles)?.id ?? 0;
    usualProfile();
  }

  /** The profile a new title gets on the place chosen: that place's own, or the usual one. */
  function usualProfile() {
    if (!adding) return;
    const of = app.profiles.filter((p) => p.kind === (adding!.kind === 'movie' ? 'movie' : 'tv'));
    const here = places.find((p) => p.id === placeId);
    const own = adding.kind === 'movie' ? here?.movie_profile : here?.series_profile;
    const usual = adding.kind === 'movie' ? app.status?.default_movie_profile : app.status?.default_series_profile;
    profileId = of.find((p) => p.id === own)?.id ?? of.find((p) => p.id === usual)?.id ?? of[0]?.id ?? 0;
  }

  async function add() {
    if (!adding) return;
    saving = true;
    const t = await act(() => api.post<Title>('/titles', { kind: adding!.kind, tmdb_id: adding!.tmdb_id, tvmaze_id: adding!.tvmaze_id, profile_id: profileId, monitored, search: searchNow && monitored, location: choices.length > 1 ? placeId : undefined }), `Added ${adding.title}`);
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
    <button class:on={kind === 'movie'} onclick={() => (kind = 'movie')}>Movies</button>
    <button class:on={kind === 'series'} onclick={() => (kind = 'series')}>Series</button>
  </div>
  <input class="input grow" type="search" placeholder="Search by title, or paste an IMDb id" bind:value={query} oninput={onInput} onkeydown={(e) => e.key === 'Enter' && run()} data-search />
</div>

{#snippet card(r: Found)}
  <div class="card result">
    <div class="art"><Poster src={r.poster} title={r.title} kind={r.kind} /></div>
    <div class="grow body">
      <div class="row wrap"><h2>{r.title}</h2><span class="muted">{r.year || ''}</span></div>
      <div class="faint meta">{[r.network ?? r.studio, r.status === 'ended' ? 'Ended' : r.status === 'continuing' ? 'Continuing' : '', r.runtime ? `${r.runtime} min` : '', r.genres.slice(0, 3).join(', '), r.anime ? `Anime ${FORMAT[r.anime.format] ?? ''}${r.anime.episodes > 1 ? `, ${r.anime.episodes} episodes` : ''}` : ''].filter(Boolean).join(' · ')}</div>
      {#if r.anime?.as_series}<p class="hint-line">Listed here as a film, and posted as its {r.anime.episodes} episodes. Add it from the series below to collect them.</p>{/if}
      <p class="overview">{r.overview}</p>
      <div class="row">
        {#if r.library_id}
          <a class="btn small" href="#/title/{r.library_id}">In library</a>
        {:else}
          <button class="btn small" class:primary={!r.anime?.as_series} onclick={() => open(r)}>Add</button>
        {/if}
      </div>
    </div>
  </div>
{/snippet}

{#if error}
  <div class="card empty">{error}{#if error.includes('TMDB')} <a class="link" href="#/settings">Open Settings</a>{/if}</div>
{:else if searching && !results.length && !others.length}
  <div class="empty">Searching…</div>
{:else if searched && !results.length}
  <div class="card empty">{kind === 'movie' && found.movies_error ? found.movies_error : `No ${kind === 'movie' ? 'movies' : 'series'} found for “${query.trim()}”.`}{#if kind === 'movie' && found.movies_error?.includes('TMDB')} <a class="link" href="#/settings">Open Settings</a>{/if}</div>
{:else if !searched}
  <div class="card empty">Find a film or a series to add to the library.</div>
{/if}

{#if results.length}<p class="faint source">{kind === 'movie' ? 'Films, from TMDB.' : 'Series, from TVmaze.'} Anime is also looked up on AniList, under any of its names.</p>{/if}
<div class="results">
  {#each results as r (r.tmdb_id ?? r.tvmaze_id)}{@render card(r)}{/each}
</div>

{#if others.length}
  <div class="section-title also">{kind === 'movie' ? 'Series' : 'Films'} with this name</div>
  <div class="results">
    {#each others.slice(0, 6) as r (r.tmdb_id ?? r.tvmaze_id)}{@render card(r)}{/each}
  </div>
{/if}

{#if adding}
  <div class="scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && (adding = null)} onkeydown={(e) => e.key === 'Escape' && (adding = null)}>
    <div class="card dialog" role="dialog" aria-modal="true" aria-label="Add {adding.title}">
      <h2>Add {adding.title} {adding.year ? `(${adding.year})` : ''}</h2>
      <label class="field">Quality profile
        <select class="input" bind:value={profileId}>
          {#each profiles as p}<option value={p.id}>{p.name}</option>{/each}
        </select>
      </label>
      {#if choices.length > 1}
        <label class="field">Keep it on
          <select class="input" bind:value={placeId} onchange={usualProfile}>
            {#each choices as p}<option value={p.id}>{p.name}{p.free != null ? ` (${bytes(p.free)} free)` : ''}</option>{/each}
          </select>
        </label>
      {/if}
      <label class="check"><input type="checkbox" bind:checked={monitored} /> Monitor, and get it when an acceptable release appears</label>
      <label class="check"><input type="checkbox" bind:checked={searchNow} disabled={!monitored || app.status?.mode !== 'active'} /> Search for it right away</label>
      {#if app.status?.mode !== 'active'}<p class="faint small">Spool is in shadow mode, so nothing will be downloaded.</p>{/if}
      {#if !profiles.length}<p class="faint small">There is no quality profile for {addingKind === 'movie' ? 'movies' : 'series'} yet. Create one in Settings first.</p>{/if}
      <div class="row end">
        <button class="btn ghost" onclick={() => (adding = null)}>Cancel</button>
        <button class="btn primary" disabled={saving || !profiles.length} onclick={add}>{saving ? 'Adding…' : 'Add to library'}</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .also {
    margin: 34px 0 12px;
  }
  .hint-line {
    margin: 6px 0 0;
    color: var(--warn, var(--accent));
    font-size: 13.5px;
  }
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
