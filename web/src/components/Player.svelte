<script lang="ts">
  // The bar along the bottom while music plays, whatever page is open.
  import { player, step, toggle, seek, stop, setVolume, resume, bookEnded, bookTime, bookSeek, bookSkip, bookChapter, setRate, sleepIn, stopBook, chapterAt } from '../lib/player.svelte';
  import { clock } from '../lib/format';
  import PlayOn from './PlayOn.svelte';

  // Something may already be playing on a player in the house.
  resume();

  const playing = $derived(player.queue[player.at]);
  const art = (id: number) => `/api/music/albums/${id}/art?size=120`;
</script>

<!-- Playing here. While music plays on a player in the house, that player is listened to instead. -->
<audio
  bind:this={player.audio}
  onplay={() => (player.book || !player.on) && (player.paused = false)}
  onpause={() => (player.book || !player.on) && (player.paused = true)}
  onended={() => (player.book ? bookEnded() : !player.on && step(1))}
  ontimeupdate={() => (player.book ? bookTime() : !player.on && (player.now = player.audio?.currentTime ?? 0))}
  ondurationchange={() => !player.book && !player.on && (player.length = player.audio && isFinite(player.audio.duration) ? player.audio.duration : (playing?.seconds ?? 0))}
></audio>

{#if player.book}
  {@const b = player.book}
  {@const ch = b.chapters[chapterAt(player.now)]}
  <!-- An audiobook: the whole book on the bar, chapter by chapter. -->
  <div class="player card" role="region" aria-label="Listening">
    <a href="#/book/{b.id}" class="cover tall" aria-label="Open {b.title}">{#if b.cover}<img src={b.cover} alt="" />{/if}</a>
    <div class="info">
      <div class="clamp"><b>{b.title}</b></div>
      <div class="muted small clamp">{ch?.title ?? b.authors.join(', ')}{player.sleepAt ? ` · stops at ${new Date(player.sleepAt).toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' })}` : ''}</div>
    </div>
    <div class="seek">
      <input type="range" min="0" max={b.seconds} step="1" value={player.now} oninput={(e) => bookSeek(Number(e.currentTarget.value))} aria-label="Place in the book" />
      <span class="faint small mono time">{clock(player.now)} / {clock(b.seconds)}</span>
    </div>
    <div class="controls">
      <button class="btn small" onclick={() => bookChapter(-1)} aria-label="Previous chapter">⏮</button>
      <button class="btn small" onclick={() => bookSkip(-30)} aria-label="Back 30 seconds">−30</button>
      <button class="btn small primary" onclick={toggle} aria-label={player.paused ? 'Play' : 'Pause'}>{player.paused ? '▶' : '❚❚'}</button>
      <button class="btn small" onclick={() => bookSkip(30)} aria-label="Forward 30 seconds">+30</button>
      <button class="btn small" onclick={() => bookChapter(1)} aria-label="Next chapter">⏭</button>
      <span class="push"></span>
      <select class="input mini" value={String(player.rate)} onchange={(e) => setRate(Number(e.currentTarget.value))} aria-label="Speed">
        {#each [0.8, 1, 1.1, 1.2, 1.3, 1.5, 1.75, 2, 2.5] as r}<option value={String(r)}>{r}×</option>{/each}
      </select>
      <select class="input mini" value="" onchange={(e) => { const v = e.currentTarget.value; sleepIn(v === 'off' ? null : Number(v)); e.currentTarget.value = ''; }} aria-label="Sleep timer">
        <option value="">{player.sleepAt ? '☾ on' : '☾'}</option>
        <option value="15">15 min</option>
        <option value="30">30 min</option>
        <option value="60">1 hour</option>
        <option value="0">End of chapter</option>
        {#if player.sleepAt}<option value="off">Off</option>{/if}
      </select>
      <button class="btn small ghost" onclick={stopBook} aria-label="Stop and close">✕</button>
    </div>
  </div>
{/if}

{#if playing && !player.book}
  <!-- One row on a wide screen; on a phone the track, then where it is in it, then the controls. -->
  <div class="player card" role="region" aria-label="Now playing">
    <a href={player.album ? `#/album/${player.album.id}` : undefined} class="cover" aria-label={player.album ? `Open ${player.album.title}` : undefined}>
      {#if player.album?.has_cover}<img src={art(player.album.id)} alt="" />{/if}
    </a>
    <div class="info">
      <div class="clamp"><b>{playing.title}</b></div>
      <div class="muted small clamp">{playing.artist}{player.album ? ` · ${player.album.title}` : ''}{#if player.on}<span class="accent"> · on {player.on.name}</span>{/if}</div>
      {#if player.error}<div class="warn small clamp">{player.error}</div>{/if}
    </div>
    <div class="seek">
      <input type="range" min="0" max={player.length || playing.seconds} step="1" value={player.now} oninput={(e) => seek(Number(e.currentTarget.value))} aria-label="Position" />
      <span class="faint small mono time">{clock(player.now)} / {clock(player.length || playing.seconds)}</span>
    </div>
    <div class="controls">
      <button class="btn small" onclick={() => step(-1)} disabled={player.at === 0} aria-label="Previous">◀◀</button>
      <button class="btn small primary" onclick={toggle} aria-label={player.paused ? 'Play' : 'Pause'}>{player.paused ? '▶' : '❚❚'}</button>
      <button class="btn small" onclick={() => step(1)} disabled={player.at >= player.queue.length - 1} aria-label="Next">▶▶</button>
      {#if player.on && player.volume != null}
        <input class="volume" type="range" min="0" max="100" step="1" value={player.volume} onchange={(e) => setVolume(Number(e.currentTarget.value))} aria-label="Volume on {player.on.name}" />
      {/if}
      <span class="push"></span>
      <PlayOn up compact />
      <button class="btn small ghost" onclick={stop} aria-label="Stop and close">✕</button>
    </div>
  </div>
{/if}

<style>
  .player {
    position: fixed;
    left: 50%;
    bottom: 12px;
    transform: translateX(-50%);
    width: min(880px, calc(100vw - 32px));
    display: grid;
    grid-template-columns: 48px minmax(0, 1fr) auto;
    grid-template-areas: 'cover info controls' 'cover seek controls';
    column-gap: 12px;
    row-gap: 2px;
    align-items: center;
    z-index: 30;
    padding: 10px 12px;
    box-shadow: 0 6px 24px rgb(0 0 0 / 0.25);
  }
  .cover {
    grid-area: cover;
    width: 48px;
    height: 48px;
    border-radius: 4px;
    overflow: hidden;
    background: var(--raised);
  }
  .cover.tall {
    height: 64px;
  }
  .input.mini {
    width: auto;
    height: 30px;
    padding: 0 6px;
    font-size: 13px;
  }
  .cover img {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
  .info {
    grid-area: info;
    min-width: 0;
  }
  .seek {
    grid-area: seek;
    display: flex;
    align-items: center;
    gap: 10px;
    min-width: 0;
  }
  .seek input {
    flex: 1;
    min-width: 0;
    margin: 0;
  }
  .time {
    white-space: nowrap;
  }
  .controls {
    grid-area: controls;
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .push {
    display: none;
  }
  .clamp {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  input.volume {
    width: 80px;
    margin: 0;
  }
  .accent {
    color: var(--accent);
  }
  .warn {
    color: var(--warn);
  }
  /* A phone: above the tab bar, and in three rows. */
  @media (max-width: 760px) {
    .player {
      bottom: calc(76px + env(safe-area-inset-bottom));
      grid-template-columns: 44px minmax(0, 1fr);
      grid-template-areas: 'cover info' 'seek seek' 'controls controls';
      row-gap: 8px;
    }
    .cover {
      width: 44px;
      height: 44px;
    }
    .push {
      display: block;
      flex: 1;
    }
    .volume {
      display: none;
    }
  }
</style>
