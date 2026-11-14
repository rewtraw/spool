<script lang="ts">
  // Music: the albums in the locations' music folders, to look through and play here. Music
  // players (Feishin, Amperfy, play:Sub and the like) connect to the same library over
  // OpenSubsonic, at this server's address.
  import { api } from '../lib/api';
  import { app, route, toast } from '../lib/state.svelte';
  import { ago, bytes, clock } from '../lib/format';

  let albums = $state<any[]>([]);
  let status = $state<any>(null);
  let loaded = $state(false);
  let filter = $state('');
  let openId = $state<number | null>(null);
  let opened = $state<any>(null);

  // Albums wanted: found in MusicBrainz, then looked for, downloaded and filed by Spool.
  // Albums are found and wanted in Discover; here is what has become of them.
  let tab = $state<'library' | 'wanted'>(route.arg === 'wanted' && app.manages ? 'wanted' : 'library');
  let wants = $state<any[]>([]);

  async function loadWants() {
    if (!app.manages) return;
    const r = await api.get<{ wants: any[] }>('/music/wants').catch(() => null);
    if (r) wants = r.wants;
  }
  loadWants();

  // Something is under way: look again every few seconds until it settles.
  $effect(() => {
    if (tab !== 'wanted' || !wants.some((w) => w.want.state !== 'have')) return;
    const t = setInterval(loadWants, 5000);
    return () => clearInterval(t);
  });

  async function again(id: number) {
    await api.post(`/music/wants/${id}/search`, {}).catch((e) => toast(e.message));
    loadWants();
  }

  async function unwant(id: number) {
    await api.del(`/music/wants/${id}`).catch((e) => toast(e.message));
    loadWants();
  }

  const WANT_LABEL: Record<string, string> = { wanted: 'Wanted', downloading: 'Downloading', have: 'Here' };

  // What is playing: the album's tracks in order, and which one.
  let queue = $state<any[]>([]);
  let at = $state(0);
  let playingAlbum = $state<any>(null);
  let audio = $state<HTMLAudioElement | null>(null);
  let paused = $state(true);
  let now = $state(0);
  let length = $state(0);

  async function load() {
    const r = await api.get<{ albums: any[]; status: any }>('/music').catch(() => null);
    if (r) {
      albums = r.albums;
      status = r.status;
    }
    loaded = true;
  }
  // Opened at an album (#/music/12), as the library links to them.
  load().then(() => {
    const id = Number(route.arg);
    if (!id || !albums.some((a) => a.id === id)) return;
    show(id);
    setTimeout(() => document.getElementById(`album-${id}`)?.scrollIntoView({ block: 'center' }), 60);
  });

  $effect(() => {
    if (!status?.scan?.scanning) return;
    const t = setInterval(load, 3000);
    return () => clearInterval(t);
  });

  async function scan() {
    try {
      await api.post('/music/scan', {});
      status = { ...status, scan: { ...status?.scan, scanning: true } };
    } catch (e: any) {
      toast(e.message);
    }
  }

  async function show(id: number) {
    if (openId === id) {
      openId = null;
      return;
    }
    openId = id;
    opened = null;
    opened = await api.get<any>(`/music/albums/${id}`).catch((e) => (toast(e.message), null));
  }

  // Browsers play FLAC, MP3, AAC, Opus and WAV themselves; anything else is made MP3 on the way.
  const plays = ['flac', 'mp3', 'm4a', 'mp4', 'aac', 'ogg', 'opus', 'wav'];
  const src = (t: any) => `/api/music/tracks/${t.id}/play${plays.includes(t.suffix) ? '' : '?format=mp3'}`;

  function playFrom(album: any, tracks: any[], i: number) {
    queue = tracks;
    playingAlbum = album;
    at = i;
    start();
  }

  function start() {
    if (!audio || !queue[at]) return;
    audio.src = src(queue[at]);
    audio.play().catch(() => {});
  }

  function next(step = 1) {
    if (at + step < 0 || at + step >= queue.length) {
      paused = true;
      return;
    }
    at += step;
    start();
  }

  function toggle() {
    if (!audio || !queue.length) return;
    if (audio.paused) audio.play().catch(() => {});
    else audio.pause();
  }

  function seek(e: Event) {
    if (audio) audio.currentTime = Number((e.target as HTMLInputElement).value);
  }

  const shown = $derived.by(() => {
    const words = filter.trim().toLowerCase().split(/\s+/).filter(Boolean);
    if (!words.length) return albums;
    return albums.filter((a) => {
      const text = `${a.title} ${a.artist} ${a.year || ''} ${a.genre}`.toLowerCase();
      return words.every((w) => text.includes(w));
    });
  });

  const quality = (a: any) => {
    const f = (a.format || '').toUpperCase();
    if (a.bit_depth && a.sample_rate) return `${f} ${a.bit_depth}/${(a.sample_rate / 1000).toFixed(1).replace(/\.0$/, '')}`;
    return f;
  };
  const minutes = (s: number) => `${Math.round(s / 60)} min`;
  const art = (id: number, size = 300) => `/api/music/albums/${id}/art?size=${size}`;
  const playing = $derived(queue[at]);
  const discs = (tracks: any[]) => new Set(tracks.map((t) => t.disc)).size > 1;
</script>

<header class="head">
  <h1>Music</h1>
</header>

{#if status}
  <div class="card strip">
    <div class="grow">
      {#if status.roots.length}
        <b>{status.albums.toLocaleString()}</b> <span class="muted">albums · {status.tracks.toLocaleString()} tracks · {bytes(status.bytes)}</span>
        <div class="faint small">
          {#if status.scan.scanning}Reading the music folders… {status.scan.read ? `${status.scan.read} files read` : ''}
          {:else if status.scan.last_at}Read {ago(status.scan.last_at)}{/if}
          {#each status.roots as r}<span class="mono"> · {r.path}</span>{#if !r.online}<span class="warn"> (cannot be reached)</span>{/if}{/each}
        </div>
        {#if status.scan.error}<div class="warn small">The last read stopped: {status.scan.error}</div>{/if}
        <div class="faint small">Music players connect over OpenSubsonic at <span class="mono">{location.origin}</span>, with your Spool name and password. Turn on the app's plain-password (legacy) sign-in.</div>
      {:else}
        <b>No music folder yet.</b> <span class="muted">Add one to a location in <a class="link" href="#/settings/library">Settings → Library</a>.</span>
      {/if}
    </div>
    {#if app.manages && status.roots.length}<button class="btn small" disabled={status.scan.scanning} onclick={scan}>{status.scan.scanning ? 'Reading…' : 'Read again'}</button>{/if}
  </div>
{/if}

{#if app.manages}
  <div class="tabs" role="tablist">
    <button class="btn small" class:primary={tab === 'library'} role="tab" aria-selected={tab === 'library'} onclick={() => (tab = 'library')}>Library</button>
    <button class="btn small" class:primary={tab === 'wanted'} role="tab" aria-selected={tab === 'wanted'} onclick={() => (tab = 'wanted')}>Wanted{wants.filter((w) => w.want.state !== 'have').length ? ` · ${wants.filter((w) => w.want.state !== 'have').length}` : ''}</button>
  </div>
{/if}

{#if tab === 'wanted'}
  <div class="row between">
    <div class="section-title">Wanted albums</div>
    <a class="btn small primary" href="#/discover?kind=music">Find albums</a>
  </div>
  {#if !wants.length}
    <div class="card empty">Nothing wanted yet. Find an album in Discover. Spool looks for a FLAC copy, or 320k when there is none, and files it in the music folder.</div>
  {:else}
    <div class="card list">
      {#each wants as { want: w, cover } (w.id)}
        <div class="line">
          <img class="thumb" src={cover} alt="" loading="lazy" onerror={(e) => ((e.currentTarget as HTMLImageElement).style.visibility = 'hidden')} />
          <div class="grow">
            <div class="clamp"><b>{w.title}</b> <span class="muted">{w.artist}{w.year ? ` · ${w.year}` : ''}</span></div>
            {#if w.release}<div class="mono faint small clamp">{w.release.title}</div>{/if}
            {#if w.state === 'have' && w.folder}<div class="mono faint small clamp">{w.folder}</div>{/if}
            {#if w.note}<div class="small" class:warn={w.state === 'wanted' && !/^Looking/.test(w.note)}>{w.note}</div>{/if}
            <div class="faint small">Wanted {ago(w.added_at)}{w.tracks ? ` · ${w.tracks[0] === w.tracks[1] ? w.tracks[0] : `${w.tracks[0]}–${w.tracks[1]}`} tracks` : ''}</div>
          </div>
          <span class="chip" class:ok={w.state === 'have'}>{WANT_LABEL[w.state]}</span>
          <div class="buttons">
            {#if w.state === 'wanted'}<button class="btn small" onclick={() => again(w.id)}>Look now</button>{/if}
            <button class="btn small ghost danger" onclick={() => unwant(w.id)}>{w.state === 'have' ? 'Clear' : 'Remove'}</button>
          </div>
        </div>
      {/each}
    </div>
  {/if}
  <div class="spacer"></div>
{:else}
{#if albums.length}
  <div class="bar">
    <input class="input" type="search" placeholder="Filter by album, artist, year or genre" bind:value={filter} aria-label="Filter albums" />
  </div>
{/if}

{#if !loaded}
  <div class="empty">Loading…</div>
{:else if status?.roots.length && !albums.length}
  <div class="card empty">{status.scan.scanning ? 'Reading the music folders…' : 'No albums found yet.'}</div>
{:else}
  <div class="grid">
    {#each shown as a (a.id)}
      <div class="album" id="album-{a.id}" class:open={openId === a.id}>
        <button class="cover" onclick={() => show(a.id)} aria-label="{a.title} by {a.artist}">
          {#if a.has_cover}<img src={art(a.id)} alt="" loading="lazy" />{/if}
          <span class="nocover">{a.title}</span>
        </button>
        <b class="clamp">{a.title}</b>
        <span class="muted small clamp">{a.artist}</span>
        <span class="faint small">{[a.year || '', quality(a)].filter(Boolean).join(' · ')}</span>
      </div>
      {#if openId === a.id}
        <div class="card detail">
          {#if !opened}
            <div class="faint">Loading…</div>
          {:else}
            <div class="row wrap top">
              {#if a.has_cover}<img class="big" src={art(a.id, 600)} alt="" />{/if}
              <div class="grow">
                <h2>{opened.album.title}</h2>
                <div class="muted">{opened.album.artist}{opened.album.year ? ` · ${opened.album.year}` : ''}{opened.album.genre ? ` · ${opened.album.genre}` : ''}</div>
                <div class="faint small">{opened.album.tracks} tracks · {minutes(opened.album.seconds)} · {bytes(opened.album.bytes)} · {quality(opened.album)}</div>
                <div class="faint small mono path">{opened.folder}</div>
                <button class="btn primary small" onclick={() => playFrom(opened.album, opened.tracks, 0)}>Play</button>
              </div>
            </div>
            <ol class="tracks">
              {#each opened.tracks as t, i (t.id)}
                <li class:now={playing?.id === t.id}>
                  <button class="track" onclick={() => playFrom(opened.album, opened.tracks, i)}>
                    <span class="n faint">{discs(opened.tracks) ? `${t.disc}-` : ''}{t.track || i + 1}</span>
                    <span class="grow clamp">{t.title}{t.artist !== opened.album.artist ? ` — ${t.artist}` : ''}</span>
                    <span class="faint small">{clock(t.seconds)}</span>
                  </button>
                </li>
              {/each}
            </ol>
          {/if}
        </div>
      {/if}
    {/each}
  </div>
{/if}
{/if}

<audio
  bind:this={audio}
  onplay={() => (paused = false)}
  onpause={() => (paused = true)}
  onended={() => next()}
  ontimeupdate={() => (now = audio?.currentTime ?? 0)}
  ondurationchange={() => (length = audio && isFinite(audio.duration) ? audio.duration : (playing?.seconds ?? 0))}
></audio>

{#if playing}
  <div class="player card">
    {#if playingAlbum.has_cover}<img src={art(playingAlbum.id, 120)} alt="" />{/if}
    <div class="grow">
      <div class="clamp"><b>{playing.title}</b></div>
      <div class="muted small clamp">{playing.artist} · {playingAlbum.title}</div>
      <input type="range" min="0" max={length || playing.seconds} step="1" value={now} oninput={seek} aria-label="Position" />
    </div>
    <span class="faint small mono">{clock(now)} / {clock(length || playing.seconds)}</span>
    <button class="btn small" onclick={() => next(-1)} disabled={at === 0} aria-label="Previous">◀◀</button>
    <button class="btn small primary" onclick={toggle} aria-label={paused ? 'Play' : 'Pause'}>{paused ? '▶' : '❚❚'}</button>
    <button class="btn small" onclick={() => next(1)} disabled={at >= queue.length - 1} aria-label="Next">▶▶</button>
  </div>
{/if}

<style>
  .head {
    margin-bottom: 12px;
  }
  .strip {
    display: flex;
    gap: 12px;
    align-items: flex-start;
    margin-bottom: 16px;
  }
  .warn {
    color: var(--warn);
  }
  .bar {
    margin-bottom: 16px;
  }
  .bar .input {
    width: 100%;
  }
  .between {
    justify-content: space-between;
    align-items: center;
    margin-bottom: 10px;
  }
  .between .section-title {
    margin: 0;
  }
  .tabs {
    display: flex;
    gap: 6px;
    margin-bottom: 14px;
  }
  .section-title {
    margin: 22px 0 10px;
  }
  .list {
    display: flex;
    flex-direction: column;
    gap: 0;
    padding-top: 4px;
    padding-bottom: 4px;
  }
  .spacer {
    height: 96px;
  }
  .line {
    display: flex;
    gap: 12px;
    align-items: center;
    padding: 10px 0;
    border-top: 1px solid var(--line);
  }
  .line:first-child {
    border-top: 0;
  }
  .thumb {
    width: 52px;
    height: 52px;
    flex: none;
    border-radius: 4px;
    object-fit: cover;
    background: var(--raised);
  }
  .buttons {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
    justify-content: flex-end;
  }
  @media (max-width: 560px) {
    .line {
      flex-wrap: wrap;
    }
    .line .buttons {
      width: 100%;
    }
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(150px, 1fr));
    gap: 18px 14px;
    padding-bottom: 96px;
  }
  .album {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .cover {
    position: relative;
    aspect-ratio: 1;
    padding: 0;
    border: 0;
    border-radius: 6px;
    overflow: hidden;
    background: var(--raised);
    cursor: pointer;
    margin-bottom: 6px;
  }
  .album.open .cover {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .cover img {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    object-fit: cover;
    z-index: 1;
  }
  .nocover {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 10px;
    text-align: center;
    font-size: 13px;
    color: var(--muted);
  }
  .clamp {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }
  .detail {
    grid-column: 1 / -1;
  }
  .detail .top {
    gap: 16px;
    align-items: flex-start;
  }
  .big {
    width: 180px;
    max-width: 40vw;
    aspect-ratio: 1;
    object-fit: cover;
    border-radius: 6px;
  }
  .detail h2 {
    margin: 0 0 4px;
  }
  .detail .btn {
    margin-top: 10px;
  }
  .path {
    word-break: break-all;
  }
  .tracks {
    list-style: none;
    padding: 0;
    margin: 14px 0 0;
  }
  .track {
    display: flex;
    gap: 10px;
    align-items: baseline;
    width: 100%;
    padding: 7px 4px;
    border: 0;
    border-top: 1px solid var(--line);
    background: none;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  .track:hover {
    background: var(--raised);
  }
  li.now .track {
    font-weight: 600;
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
  .player {
    position: fixed;
    left: 50%;
    bottom: 12px;
    transform: translateX(-50%);
    width: min(720px, calc(100vw - 32px));
    display: flex;
    gap: 10px;
    align-items: center;
    z-index: 20;
    box-shadow: 0 6px 24px rgb(0 0 0 / 0.18);
  }
  .player img {
    width: 48px;
    height: 48px;
    border-radius: 4px;
    object-fit: cover;
  }
  .player input[type='range'] {
    width: 100%;
    margin: 4px 0 0;
  }
  @media (max-width: 560px) {
    .player .mono {
      display: none;
    }
  }
</style>
