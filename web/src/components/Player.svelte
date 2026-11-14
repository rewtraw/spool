<script lang="ts">
  // The bar along the bottom while music plays, whatever page is open.
  import { player, step, toggle, seek, stop } from '../lib/player.svelte';
  import { clock } from '../lib/format';

  const playing = $derived(player.queue[player.at]);
  const art = (id: number) => `/api/music/albums/${id}/art?size=120`;
</script>

<audio
  bind:this={player.audio}
  onplay={() => (player.paused = false)}
  onpause={() => (player.paused = true)}
  onended={() => step(1)}
  ontimeupdate={() => (player.now = player.audio?.currentTime ?? 0)}
  ondurationchange={() => (player.length = player.audio && isFinite(player.audio.duration) ? player.audio.duration : (playing?.seconds ?? 0))}
></audio>

{#if playing}
  <div class="player card" role="region" aria-label="Now playing">
    <a href="#/album/{player.album.id}" class="cover" aria-label="Open {player.album.title}">
      {#if player.album.has_cover}<img src={art(player.album.id)} alt="" />{/if}
    </a>
    <div class="grow">
      <div class="clamp"><b>{playing.title}</b></div>
      <div class="muted small clamp">{playing.artist} · {player.album.title}</div>
      <input type="range" min="0" max={player.length || playing.seconds} step="1" value={player.now} oninput={(e) => seek(Number(e.currentTarget.value))} aria-label="Position" />
    </div>
    <span class="faint small mono time">{clock(player.now)} / {clock(player.length || playing.seconds)}</span>
    <button class="btn small" onclick={() => step(-1)} disabled={player.at === 0} aria-label="Previous">◀◀</button>
    <button class="btn small primary" onclick={toggle} aria-label={player.paused ? 'Play' : 'Pause'}>{player.paused ? '▶' : '❚❚'}</button>
    <button class="btn small" onclick={() => step(1)} disabled={player.at >= player.queue.length - 1} aria-label="Next">▶▶</button>
    <button class="btn small ghost" onclick={stop} aria-label="Stop and close">✕</button>
  </div>
{/if}

<style>
  .player {
    position: fixed;
    left: 50%;
    bottom: 12px;
    transform: translateX(-50%);
    width: min(720px, calc(100vw - 32px));
    display: flex;
    gap: 10px;
    align-items: center;
    z-index: 30;
    box-shadow: 0 6px 24px rgb(0 0 0 / 0.25);
  }
  .cover {
    width: 48px;
    height: 48px;
    flex: none;
    border-radius: 4px;
    overflow: hidden;
    background: var(--raised);
  }
  .cover img {
    width: 100%;
    height: 100%;
    object-fit: cover;
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
  input[type='range'] {
    width: 100%;
    margin: 4px 0 0;
  }
  /* Above the tab bar along the bottom on a phone. */
  @media (max-width: 760px) {
    .player {
      bottom: calc(76px + env(safe-area-inset-bottom));
    }
    .time {
      display: none;
    }
  }
</style>
