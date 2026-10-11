<script lang="ts">
  import { app, act, go, loadTitles } from '../lib/state.svelte';
  import { api } from '../lib/api';
  import Poster from '../components/Poster.svelte';

  let { id }: { id: number } = $props();

  const saved = JSON.parse(localStorage.getItem('spool.collection') ?? '{}');
  let order = $state<'release' | 'story'>(saved.order ?? 'release');
  $effect(() => localStorage.setItem('spool.collection', JSON.stringify({ order })));

  const c = $derived(app.collections.find((x) => x.id === id));
  const members = $derived(
    (c?.members ?? [])
      .map((m) => ({ ...m, t: app.titles.find((t) => t.id === m.title_id) }))
      .filter((m) => m.t)
      .sort((a, b) => (order === 'story' ? a.story - b.story || a.year - b.year : a.year - b.year || a.title_id - b.title_id)),
  );
  const missing = $derived([...(c?.missing ?? [])].sort((a, b) => (order === 'story' ? a.story - b.story || a.year - b.year : a.year - b.year)));
  // Whether following the story differs from following the releases at all.
  const twoOrders = $derived(c?.kind === 'anime' && (c?.members ?? []).some((m, i, all) => all.some((o, j) => j > i && o.story < m.story)));
  const next = $derived(c?.next ? app.titles.find((t) => t.id === c.next) : undefined);
  const seen = $derived((c?.members ?? []).filter((m) => m.watched).length);
  const FORMAT: Record<string, string> = { TV: 'Series', TV_SHORT: 'Short series', MOVIE: 'Film', OVA: 'OVA', ONA: 'Web series', SPECIAL: 'Special' };
  const ROLES = ['', 'Recap', 'Side story', 'Alternative telling', 'Spin-off'];
  let adding = $state<Record<string, boolean>>({});
  let editing = $state(false);
  let name = $state('');

  const refresh = () => loadTitles(true);

  async function add(m: { anilist_id: number; tmdb_id?: number | null; title: string }) {
    const key = `${m.anilist_id}-${m.tmdb_id ?? 0}`;
    adding[key] = true;
    await act(() => api.post('/collections/add', m.tmdb_id ? { tmdb_id: m.tmdb_id } : { anilist_id: m.anilist_id }), `Added ${m.title}. Looking for it.`);
    adding[key] = false;
    loadTitles();
  }
  async function rename() {
    await act(() => api.put(`/collections/${id}`, { name }), 'Renamed');
    refresh();
  }
  async function setRole(titleId: number, role: string) {
    await act(() => api.put(`/titles/${titleId}/collection`, { role }));
    refresh();
  }
  async function takeOut(titleId: number) {
    await act(() => api.put(`/titles/${titleId}/collection`, { collection: 0 }), 'Taken out of the collection');
    refresh();
  }
  async function undo() {
    if (!confirm(`Remove the collection “${c?.name}”? Its titles stay in the library.`)) return;
    await act(() => api.del(`/collections/${id}`), 'Collection removed');
    await refresh();
    go('library');
  }
</script>

{#if !app.titlesLoaded}
  <div class="empty">Loading…</div>
{:else if !c}
  <div class="card empty">This collection has fewer than two titles in the library now. <a href="#/library" style="color:var(--accent)">Back to the library</a></div>
{:else}
  <header class="head">
    <div class="grow">
      <a class="back" href="#/library">Library</a>
      <h1>{c.name}</h1>
      <div class="muted">
        {members.length} of {members.length + c.missing.length} in the library{seen ? ` · ${seen} watched` : ''}{c.kind === 'own' ? ' · your own collection' : ''}
      </div>
    </div>
    <div class="row wrap tools">
      {#if twoOrders}
        <div class="seg" aria-label="Order">
          <button class:on={order === 'release'} aria-pressed={order === 'release'} onclick={() => (order = 'release')}>Release order</button>
          <button class:on={order === 'story'} aria-pressed={order === 'story'} onclick={() => (order = 'story')}>Story order</button>
        </div>
      {/if}
      <button class="btn small" class:primary={editing} onclick={() => { editing = !editing; name = c?.name ?? ''; }}>{editing ? 'Done' : 'Edit'}</button>
    </div>
  </header>

  {#if editing}
    <div class="card edit">
      <label class="field">Name<div class="row"><input class="input grow" bind:value={name} /><button class="btn" onclick={rename}>Save</button></div>
        <span class="hint">{c.kind === 'own' ? 'Your own collection.' : 'Leave empty and save to give back the name its source has.'} Below, say what each title is to the story, or take it out. Your changes stand over what TMDB and AniList say.</span>
      </label>
      {#if c.kind === 'own'}<div><button class="btn small ghost danger" onclick={undo}>Remove this collection</button></div>{/if}
    </div>
  {/if}

  {#if next}
    <a class="card upnext" href="#/title/{next.id}">
      <span class="faint small">Up next{order === 'story' || !twoOrders ? '' : ', in story order'}</span>
      <b>{next.title}</b> <span class="muted">{next.year || ''}</span>
    </a>
  {/if}

  <div class="grid">
    {#each members as m (m.title_id)}
      {@const t = m.t!}
      <div class="tile">
        <a href="#/title/{t.id}" class:dim={t.offline}>
          <div class="art">
            <Poster src={t.poster_small ?? t.poster} title={t.title} kind={t.kind} />
            {#if m.role}<span class="chip pin">{m.role}{m.role === 'Recap' ? ' · can be skipped' : ''}</span>{/if}
            {#if m.watched}<span class="chip ok seen" title="Watched">✓</span>{/if}
          </div>
          <div class="name truncate">{t.title}</div>
          <div class="faint small">{t.year || ''} · {t.kind === 'series' ? 'Series' : 'Film'}</div>
        </a>
        {#if editing}
          <select class="input small" value={m.role} onchange={(e) => setRole(t.id, e.currentTarget.value)} aria-label="What {t.title} is to the story">
            {#each ROLES as r}<option value={r}>{r || 'Part of the story'}</option>{/each}
          </select>
          <button class="btn small ghost danger" onclick={() => takeOut(t.id)}>Take out</button>
        {/if}
      </div>
    {/each}
  </div>

  {#if missing.length}
    <div class="section-title rest">Not in the library</div>
    <p class="muted small note">{c.kind === 'anime' ? 'Other works of the same story, as AniList relates them to what you have.' : 'The other films TMDB puts in this collection.'}</p>
    <div class="card list">
      {#each missing as m (`${m.anilist_id}-${m.tmdb_id ?? 0}`)}
        {@const key = `${m.anilist_id}-${m.tmdb_id ?? 0}`}
        <div class="row item">
          <div class="thumb"><Poster src={m.poster} title={m.title} kind={m.format === 'MOVIE' ? 'movie' : 'series'} /></div>
          <div class="grow">
            <b>{m.title}</b> <span class="muted">{m.year || 'Not released'}</span>
            <div class="faint small">{[FORMAT[m.format] ?? m.format, m.episodes > 1 ? `${m.episodes} episodes` : '', m.role].filter(Boolean).join(' · ')}</div>
          </div>
          {#if m.can_add}<button class="btn small" disabled={adding[key]} onclick={() => add(m)}>{adding[key] ? 'Adding…' : 'Add'}</button>
          {:else}<span class="faint small">Add it from Discover</span>{/if}
        </div>
      {/each}
    </div>
  {/if}
{/if}

<style>
  .back {
    color: var(--muted);
    font-size: 13px;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(140px, 1fr));
    gap: 18px 14px;
    margin-top: 18px;
  }
  .tile {
    display: grid;
    gap: 6px;
    min-width: 0;
    align-content: start;
  }
  .tile a {
    display: grid;
    gap: 4px;
    min-width: 0;
  }
  .tile a.dim .art {
    opacity: 0.55;
  }
  .tools {
    gap: 8px;
    align-items: center;
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
    color: var(--muted);
    border-radius: 7px;
    font: inherit;
    font-size: 13px;
    cursor: pointer;
  }
  .seg button.on {
    background: var(--surface);
    color: var(--text);
  }
  .edit {
    margin-top: 16px;
    padding: 14px 16px;
    display: grid;
    gap: 10px;
  }
  .upnext {
    display: block;
    margin-top: 16px;
    padding: 12px 16px;
  }
  .upnext .small {
    display: block;
  }
  .seen {
    position: absolute;
    top: 7px;
    right: 7px;
  }
  .input.small {
    height: 30px;
    font-size: 13px;
    padding: 0 8px;
  }
  .art {
    position: relative;
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
  }
  /* Set apart from the grid above, with room between the heading, its note and the list. */
  .rest {
    margin: 40px 0 6px;
  }
  .note {
    margin: 0 0 14px;
    line-height: 1.5;
  }
  .item {
    gap: 12px;
    padding: 10px 14px;
    align-items: center;
  }
  .thumb {
    width: 44px;
    flex: none;
  }
  @media (max-width: 600px) {
    .grid {
      grid-template-columns: repeat(3, 1fr);
      gap: 14px 10px;
    }
  }
</style>
