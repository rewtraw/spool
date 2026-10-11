<script lang="ts">
  import { app, act, loadTitles } from '../lib/state.svelte';
  import { api } from '../lib/api';
  import Poster from '../components/Poster.svelte';

  let { id }: { id: number } = $props();

  const c = $derived(app.collections.find((x) => x.id === id));
  const members = $derived((c?.members ?? []).map((m) => ({ ...m, t: app.titles.find((t) => t.id === m.title_id) })).filter((m) => m.t));
  const FORMAT: Record<string, string> = { TV: 'Series', TV_SHORT: 'Short series', MOVIE: 'Film', OVA: 'OVA', ONA: 'Web series', SPECIAL: 'Special' };
  let adding = $state<Record<number, boolean>>({});

  async function add(m: { anilist_id: number; title: string }) {
    adding[m.anilist_id] = true;
    await act(() => api.post('/collections/add', { anilist_id: m.anilist_id }), `Added ${m.title}. Looking for it.`);
    adding[m.anilist_id] = false;
    loadTitles();
  }
</script>

{#if !app.titlesLoaded}
  <div class="empty">Loading…</div>
{:else if !c}
  <div class="card empty">This collection has fewer than two titles in the library now. <a href="#/library" style="color:var(--accent)">Back to the library</a></div>
{:else}
  <header class="head">
    <div>
      <a class="back" href="#/library">Library</a>
      <h1>{c.name}</h1>
      <div class="muted">{members.length} in the library, in the order they came out{c.kind === 'anime' && c.missing.length ? ` · ${c.missing.length} more of the story not here` : ''}</div>
    </div>
  </header>

  <div class="grid">
    {#each members as m (m.title_id)}
      {@const t = m.t!}
      <a class="tile" href="#/title/{t.id}" class:dim={t.offline}>
        <div class="art">
          <Poster src={t.poster_small ?? t.poster} title={t.title} kind={t.kind} />
          {#if m.role}<span class="chip pin">{m.role}</span>{/if}
        </div>
        <div class="name truncate">{t.title}</div>
        <div class="faint small">{t.year || ''} · {t.kind === 'series' ? 'Series' : 'Film'}</div>
      </a>
    {/each}
  </div>

  {#if c.missing.length}
    <div class="section-title rest">Not in the library</div>
    <p class="muted small note">Other works of the same story, as AniList relates them to what you have.</p>
    <div class="card list">
      {#each c.missing as m (m.anilist_id)}
        <div class="row item">
          <div class="thumb"><Poster src={m.poster} title={m.title} kind={m.format === 'MOVIE' ? 'movie' : 'series'} /></div>
          <div class="grow">
            <b>{m.title}</b> <span class="muted">{m.year || ''}</span>
            <div class="faint small">{[FORMAT[m.format] ?? m.format, m.episodes > 1 ? `${m.episodes} episodes` : '', m.role].filter(Boolean).join(' · ')}</div>
          </div>
          {#if m.can_add}<button class="btn small" disabled={adding[m.anilist_id]} onclick={() => add(m)}>{adding[m.anilist_id] ? 'Adding…' : 'Add'}</button>
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
    gap: 4px;
    min-width: 0;
  }
  .tile.dim .art {
    opacity: 0.55;
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
