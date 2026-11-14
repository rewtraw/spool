<script lang="ts">
  // Letterboxd lists followed: every film on one is added to the library, and every film put
  // on it later. A film added from a list and removed later is not added again.
  import { onDestroy, untrack } from 'svelte';
  import { api } from '../lib/api';
  import { act, app, toast } from '../lib/state.svelte';
  import { ago } from '../lib/format';

  let { initial = '' }: { initial?: string } = $props();

  type List = { id: number; url: string; name: string; profile_id: number; films: number; added: number; missed: number; checked_at: number; error: string; added_at: number };

  // The link it was opened with; it is opened again for another.
  let url = $state(untrack(() => initial));
  const movieProfiles = $derived(app.profiles.filter((p) => p.kind === 'movie'));
  let profile = $state(0);
  let busy = $state(false);
  let lists = $state<List[]>([]);

  $effect(() => {
    if (!profile && movieProfiles.length) profile = app.status?.default_movie_profile || movieProfiles[0].id;
  });

  async function load() {
    try {
      lists = (await api.get<{ lists: List[] }>('/lists')).lists;
    } catch {}
  }
  // A list just followed is read in the background; its count fills in as it is.
  let timer: ReturnType<typeof setTimeout> | null = null;
  async function tick() {
    await load();
    timer = setTimeout(tick, lists.some((l) => !l.checked_at) ? 5000 : 60000);
  }
  tick();
  onDestroy(() => timer && clearTimeout(timer));

  async function follow() {
    busy = true;
    try {
      const l = await api.post<List>('/lists', { url: url.trim(), profile_id: profile });
      toast(`Following ${l.name}. Its films are being added.`);
      url = '';
      await load();
    } catch (e: any) {
      toast(e.message, 'error');
    } finally {
      busy = false;
    }
  }
</script>

<form class="card add" onsubmit={(e) => { e.preventDefault(); follow(); }}>
  <div class="row wrap">
    <input class="input grow mono" type="url" inputmode="url" placeholder="https://letterboxd.com/…/list/…/ or …/watchlist/" bind:value={url} aria-label="Letterboxd list" data-search />
    <select class="input auto" bind:value={profile} aria-label="Quality profile for the films it adds">
      {#each movieProfiles as p}<option value={p.id}>{p.name}</option>{/each}
    </select>
    <button class="btn primary" type="submit" disabled={busy || !url.trim()}>{busy ? 'Reading…' : 'Follow'}</button>
  </div>
  <p class="faint small">Any public list or watchlist on Letterboxd: a critic's, a festival's, the Criterion Collection, “1001 Movies You Must See”. Every film on it the library lacks is added and looked for, and films put on it later are added as they appear. A film you remove is not added again.</p>
</form>

{#if lists.length}
  <div class="stack">
    {#each lists as l (l.id)}
      <div class="card item">
        <div class="grow">
          <a class="t" href={l.url} target="_blank" rel="noreferrer">{l.name}</a>
          <div class="muted small">
            {#if !l.checked_at}Being read for the first time{l.added ? `: ${l.added} added so far` : ''}…
            {:else}{l.films} {l.films === 1 ? 'film' : 'films'} · {l.added} added by it{l.missed ? ` · ${l.missed} not found on TMDB` : ''} · read {ago(l.checked_at)}{/if}
          </div>
          {#if l.error}<div class="err">{l.error}</div>{/if}
        </div>
        <div class="buttons">
          <select class="input auto small" value={l.profile_id || app.status?.default_movie_profile} onchange={async (e) => { await act(() => api.patch(`/lists/${l.id}`, { profile_id: Number(e.currentTarget.value) }), 'Saved'); load(); }} aria-label="Quality profile for the films it adds">
            {#each movieProfiles as p}<option value={p.id}>{p.name}</option>{/each}
          </select>
          <button class="btn small" onclick={async () => { await act(() => api.post(`/lists/${l.id}/read`, {}), 'Reading it again'); load(); }}>Read now</button>
          <button class="btn small ghost danger" onclick={async () => { await act(() => api.del(`/lists/${l.id}`), `No longer following ${l.name}. What it added stays.`); load(); }}>Stop following</button>
        </div>
      </div>
    {/each}
  </div>
  <p class="faint small">Lists are read again every six hours.</p>
{/if}

<style>
  .add { padding: 14px; display: grid; gap: 10px; margin-bottom: 18px; }
  .item { display: flex; gap: 12px; padding: 12px 14px; align-items: center; flex-wrap: wrap; }
  .item .t { font-weight: 600; }
  .buttons { display: flex; gap: 6px; flex-wrap: wrap; align-items: center; }
  .err { color: var(--bad); font-size: 0.9em; }
</style>
