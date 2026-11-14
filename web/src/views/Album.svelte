<script lang="ts">
  // One album in the music folder: its tracks, to play here. Music players connect to the same
  // library over OpenSubsonic.
  import { api } from '../lib/api';
  import { toast } from '../lib/state.svelte';
  import { bytes, clock } from '../lib/format';
  import { player, playFrom } from '../lib/player.svelte';

  let { id }: { id: number } = $props();
  let opened = $state<any>(null);
  let failed = $state(false);
  // The page is made anew for each album (see App), so it is read once.
  function load() {
    api.get<any>(`/music/albums/${id}`).then(
      (r) => (opened = r),
      (e) => ((failed = true), toast(e.message)),
    );
  }
  load();

  const quality = (a: any) => {
    const f = (a.format || '').toUpperCase();
    if (a.bit_depth && a.sample_rate) return `${f} ${a.bit_depth}/${(a.sample_rate / 1000).toFixed(1).replace(/\.0$/, '')}`;
    return f;
  };
  const minutes = (s: number) => `${Math.round(s / 60)} min`;
  const discs = (tracks: any[]) => new Set(tracks.map((t) => t.disc)).size > 1;
  const playing = $derived(player.album?.id === id ? player.queue[player.at] : null);
</script>

<p class="back"><a class="link" href="#/library?kind=music">← Music</a></p>

{#if failed}
  <div class="card empty">This album is not in the music folder any more.</div>
{:else if !opened}
  <div class="empty">Loading…</div>
{:else}
  {@const a = opened.album}
  <div class="top">
    <div class="cover">{#if a.has_cover}<img src="/api/music/albums/{id}/art?size=600" alt="" />{/if}</div>
    <div class="about">
      <h1>{a.title}</h1>
      <div class="muted">{a.artist}{a.year ? ` · ${a.year}` : ''}{a.genre ? ` · ${a.genre}` : ''}</div>
      <div class="faint small">{a.tracks} tracks · {minutes(a.seconds)} · {bytes(a.bytes)} · {quality(a)}</div>
      <div class="faint small mono path">{opened.folder}</div>
      <div><button class="btn primary" onclick={() => playFrom(a, opened.tracks, 0)}>Play</button></div>
    </div>
  </div>
  <ol class="card tracks">
    {#each opened.tracks as t, i (t.id)}
      <li class:now={playing?.id === t.id}>
        <button class="track" onclick={() => playFrom(a, opened.tracks, i)}>
          <span class="n faint">{discs(opened.tracks) ? `${t.disc}-` : ''}{t.track || i + 1}</span>
          <span class="grow clamp">{t.title}{t.artist !== a.artist ? ` — ${t.artist}` : ''}</span>
          <span class="faint small">{clock(t.seconds)}</span>
        </button>
      </li>
    {/each}
  </ol>
{/if}

<style>
  .back {
    margin: 0 0 14px;
  }
  .top {
    display: flex;
    gap: 20px;
    align-items: flex-end;
    flex-wrap: wrap;
    margin-bottom: 20px;
  }
  .cover {
    width: 220px;
    max-width: 45vw;
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
    gap: 4px;
    flex: 1 1 260px;
    min-width: 0;
  }
  .about h1 {
    margin: 0;
  }
  .about .btn {
    margin-top: 10px;
  }
  .path {
    word-break: break-all;
  }
  .tracks {
    list-style: none;
    padding: 4px 12px;
    margin: 0 0 110px;
  }
  .track {
    display: flex;
    gap: 10px;
    align-items: baseline;
    width: 100%;
    padding: 9px 4px;
    border: 0;
    border-top: 1px solid var(--line);
    background: none;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  li:first-child .track {
    border-top: 0;
  }
  @media (hover: hover) {
    .track:hover {
      background: var(--raised);
    }
  }
  li.now .track {
    font-weight: 600;
    color: var(--accent);
  }
  .n {
    width: 2.5em;
    flex: none;
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .grow {
    flex: 1;
    min-width: 0;
  }
  .clamp {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
