<script lang="ts">
  // Cut a clip from a film, or from one episode of a series.
  import { api, type Episode, type Watched } from '../lib/api';
  import { clock } from '../lib/format';
  import MomentFinder from './MomentFinder.svelte';

  let { titleId, kind, episodes = [], watched = {}, filmPosition = 0 }: { titleId: number; kind: 'movie' | 'series'; episodes?: Episode[]; watched?: Record<number, Watched>; filmPosition?: number } = $props();

  let open = $state(false);
  let finding = $state(false);
  let start = $state('');
  let seconds = $state(30);
  let name = $state('');
  let subtitles = $state('auto');
  let error = $state('');
  let made = $state<{ name: string } | null>(null);

  const held = $derived(episodes.filter((e) => e.file_id).sort((a, b) => a.season - b.season || a.episode - b.episode));
  // The episode someone stopped part-way through, as the likeliest one wanted.
  const stoppedIn = $derived(held.find((e) => watched[e.id] && !watched[e.id].done && (watched[e.id].position ?? 0) > 0));
  let episodeId = $state(0);
  $effect(() => {
    if (!episodeId && held.length) episodeId = (stoppedIn ?? held[0]).id;
  });
  const stoppedAt = $derived(kind === 'movie' ? filmPosition : watched[episodeId] && !watched[episodeId].done ? (watched[episodeId].position ?? 0) : 0);
  const label = (e: Episode) => `S${String(e.season).padStart(2, '0')}E${String(e.episode).padStart(2, '0')}${e.title ? ` · ${e.title}` : ''}`;

  async function make(e: Event) {
    e.preventDefault();
    error = '';
    made = null;
    try {
      const body: any = { title_id: titleId, start: start.trim(), seconds: Number(seconds) || 30 };
      if (kind === 'series') body.episode_id = episodeId;
      if (name.trim()) body.name = name.trim();
      body.subtitles = subtitles;
      const clip = await api.post('/clips', body);
      made = { name: clip.name };
      name = '';
    } catch (err: any) {
      error = err.message;
    }
  }
</script>

{#if finding}
  <MomentFinder {titleId} onclose={() => (finding = false)} />
{:else if !open}
  <div class="row wrap">
    <button class="btn small ghost" onclick={() => (open = true)} aria-expanded="false">Make a clip</button>
    <button class="btn small ghost" onclick={() => (finding = true)} aria-expanded="false">Find what is said</button>
  </div>
{:else}
  <form class="card maker" onsubmit={make}>
    <div class="top">
      <b>Make a clip</b>
      <button type="button" class="btn small ghost" onclick={() => (open = false)}>Close</button>
    </div>
    {#if kind === 'series'}
      <label class="field">Episode
        <select class="input" bind:value={episodeId}>
          {#each held as e (e.id)}<option value={e.id}>{label(e)}</option>{/each}
        </select>
      </label>
    {/if}
    <div class="two">
      <label class="field">Starts at
        <input class="input" bind:value={start} placeholder="30m or 1:02:05" autocomplete="off" autocapitalize="none" spellcheck="false" required />
      </label>
      <label class="field">Seconds long
        <input class="input" type="number" min="1" max="600" step="1" bind:value={seconds} required />
      </label>
    </div>
    <div class="row wrap quick">
      {#if stoppedAt > 1}<button type="button" class="btn small ghost" onclick={() => (start = clock(Math.max(0, stoppedAt - seconds)))} title="Starts {clock(seconds)} before where you stopped, so it ends there">Up to where I stopped ({clock(stoppedAt)})</button>{/if}
      {#each [15, 30, 60] as n}<button type="button" class="btn small ghost" class:on={Number(seconds) === n} onclick={() => (seconds = n)}>{n} s</button>{/each}
    </div>
    <label class="field">Subtitles
      <select class="input" bind:value={subtitles}>
        <option value="auto">Automatic: when the speech is in another language</option>
        <option value="on">Always</option>
        <option value="none">None</option>
      </select>
    </label>
    <label class="field">Name, if not the title and the time
      <input class="input" bind:value={name} autocomplete="off" />
    </label>
    {#if error}<p class="error">{error}</p>{/if}
    {#if made}<p class="small">Making “{made.name}”. It will be under <a class="link" href="#/clips">Clips</a> in a moment.</p>{/if}
    <div class="row go">
      <button class="btn primary" type="submit" disabled={kind === 'series' && !episodeId}>Make it</button>
      <span class="faint small">HD video that plays anywhere you send it, up to ten minutes.</span>
    </div>
  </form>
{/if}

<style>
  .maker {
    display: grid;
    gap: 12px;
    max-width: 520px;
    margin-top: 6px;
    padding: 14px 16px 16px;
  }
  .top {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }
  .two {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 10px;
  }
  .quick :global(.btn.on) {
    border-color: var(--accent);
    color: var(--accent);
  }
  .error {
    color: var(--bad, #d9534f);
    margin: 0;
  }
  p {
    margin: 0;
  }
  .go .btn {
    flex-shrink: 0;
    white-space: nowrap;
  }
</style>
