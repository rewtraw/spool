<script lang="ts">
  import { api, type Clip } from '../lib/api';
  import { app, act, toast } from '../lib/state.svelte';
  import { ago, bytes, clock, pct } from '../lib/format';
  import MomentFinder from '../components/MomentFinder.svelte';
  import { drawTerminalCard } from '../lib/terminal-card';

  let clips = $state<Clip[] | null>(null);
  let confirming = $state<number | null>(null);

  async function load() {
    clips = await api.get('/clips').catch(() => clips ?? []);
  }
  $effect(() => {
    app.clipTick;
    load();
  });
  // A clip being made says how far it has got only when asked.
  $effect(() => {
    if (!clips?.some((c) => c.state === 'making')) return;
    const timer = setInterval(load, 1000);
    return () => clearInterval(timer);
  });

  // Each clip's player, to go to a line of what is said.
  let players = $state<Record<number, HTMLVideoElement>>({});
  function playFrom(c: Clip, at: number) {
    const v = players[c.id];
    if (!v) return;
    v.currentTime = at;
    v.play().catch(() => {});
  }

  // What is being fetched to save, and a file fetched but not yet handed over, by clip.
  let saving = $state<Record<number, string>>({});
  let ready = $state<Record<number, File>>({});

  /** A clip's name as a file name. */
  const fileName = (c: Clip, kind: string) => `${c.name.replace(/[/\\:"'\n\r]+/g, ' ').replace(/\s+/g, ' ').trim() || `Clip ${c.id}`}.${kind}`;

  /** Hand a file to the phone's share sheet, where it can go straight to Photos; false where
   * the browser offers no such thing. */
  async function share(c: Clip, file: File) {
    try {
      await navigator.share({ files: [file] });
      delete ready[c.id];
    } catch (err: any) {
      // Fetching took long enough that the tap no longer counts: the next one hands it over.
      if (err?.name === 'NotAllowedError') ready[c.id] = file;
      else if (err?.name !== 'AbortError') toast(err.message, 'error');
    }
  }

  /** Hand a file over: to Photos through the share sheet where there is one, and as a
   * download where there is not. */
  async function deliver(c: Clip, file: File) {
    if (typeof navigator.canShare === 'function' && navigator.canShare({ files: [file] })) {
      await share(c, file);
    } else {
      const a = document.createElement('a');
      a.href = URL.createObjectURL(file);
      a.download = file.name;
      a.click();
      setTimeout(() => URL.revokeObjectURL(a.href), 60000);
    }
  }

  /** Save a clip, as its video or as a GIF. */
  async function save(c: Clip, kind: 'mp4' | 'gif') {
    const url = kind === 'gif' ? `/api/clips/${c.id}/gif` : `/api/clips/${c.id}/video`;
    if (typeof navigator.canShare !== 'function' && kind === 'mp4') {
      location.href = `${url}?download`;
      return;
    }
    saving[c.id] = kind === 'gif' ? 'Making the GIF…' : 'Fetching…';
    try {
      const res = await fetch(url);
      if (!res.ok) throw new Error((await res.json().catch(() => null))?.error ?? `It could not be fetched (${res.status})`);
      await deliver(c, new File([await res.blob()], fileName(c, kind), { type: kind === 'gif' ? 'image/gif' : 'video/mp4' }));
    } catch (err: any) {
      toast(err.message, 'error');
    } finally {
      delete saving[c.id];
    }
  }

  // Lines of what is said chosen for a picture: one clip at a time, by their place in it.
  let choosing = $state<number | null>(null);
  let chosen = $state<number[]>([]);
  let card = $state<HTMLCanvasElement | null>(null);
  const chosenText = (c: Clip) => (c.said ?? []).filter((_, i) => chosen.includes(i)).map((l) => l.text);

  function choose(c: Clip) {
    choosing = choosing === c.id ? null : c.id;
    chosen = [];
  }
  function pick(i: number) {
    chosen = chosen.includes(i) ? chosen.filter((x) => x !== i) : [...chosen, i].sort((a, b) => a - b);
  }
  // The picture is drawn as the lines are chosen, so what is saved is what is seen.
  $effect(() => {
    const c = clips?.find((x) => x.id === choosing);
    if (card && c && chosen.length) drawTerminalCard(card, chosenText(c), c.from);
  });

  /** Save the chosen lines as a picture in the terminal style, without times. */
  async function saveCard(c: Clip) {
    if (!card || !chosen.length) return;
    drawTerminalCard(card, chosenText(c), c.from);
    const blob = await new Promise<Blob | null>((done) => card!.toBlob(done, 'image/png'));
    if (!blob) return toast('The picture could not be made', 'error');
    await deliver(c, new File([blob], fileName(c, 'png'), { type: 'image/png' }));
  }

  const linkOf = (c: Clip) => `${location.origin}/clip/${c.link}.mp4`;

  /** Put a link on the clipboard. Spool is reached over plain http, where browsers keep the
   * clipboard's own interface away from pages, so the old way is the one that works. */
  function copy(text: string) {
    const area = document.createElement('textarea');
    area.value = text;
    area.setAttribute('readonly', '');
    area.style.position = 'fixed';
    area.style.opacity = '0';
    document.body.appendChild(area);
    area.select();
    area.setSelectionRange(0, text.length);
    let done = false;
    try {
      done = document.execCommand('copy');
    } catch {
      done = false;
    }
    area.remove();
    return done;
  }

  function copyLink(c: Clip) {
    if (copy(linkOf(c))) toast('Link copied. It plays the clip for anyone who can reach Spool, without signing in.');
    else toast('The link could not be copied; open it and copy it from the address bar.', 'error');
  }

  async function remove(c: Clip) {
    confirming = null;
    await act(() => api.del(`/clips/${c.id}`), 'Clip removed');
    load();
  }

  const size = (c: Clip) => [c.height ? `${c.height}p` : '', c.subtitles_shown ? `${c.subtitles_shown} subtitles` : '', bytes(c.size)].filter(Boolean).join(' · ');
</script>

<header class="head">
  <h1>Clips</h1>
</header>
<p class="muted lede">Moments cut from films and episodes, made to play anywhere they are sent. Find them here by what is said in them, or make one from a film's or a series' page, under <b>Make a clip</b>.</p>

<MomentFinder onmade={load} />

{#if clips === null}
  <div class="empty">Loading…</div>
{:else if !clips.length}
  <div class="card empty">No clips yet. Open a film or a series in the <a class="link" href="#/library">library</a> and choose <b>Make a clip</b>.</div>
{:else}
  <div class="clips">
    {#each clips as c (c.id)}
      <article class="card clip">
        <div class="frame">
          {#if c.state === 'ready'}
            <!-- svelte-ignore a11y_media_has_caption -->
            <video bind:this={players[c.id]} src="/api/clips/{c.id}/video" poster="/api/clips/{c.id}/still" controls playsinline preload="none"></video>
          {:else if c.state === 'making'}
            <div class="placeholder">
              <span>{c.progress ? `Making it… ${Math.round(pct(c.progress, 1))}%` : 'Waiting to be made…'}</span>
              <div class="progress" role="progressbar" aria-label="How far it has got" aria-valuenow={Math.round(pct(c.progress ?? 0, 1))} aria-valuemin="0" aria-valuemax="100"><i style="width:{pct(c.progress ?? 0, 1)}%"></i></div>
            </div>
          {:else}
            <div class="placeholder bad"><span>It could not be made: {c.error}</span></div>
          {/if}
        </div>
        <div class="about">
          <b class="name">{c.name}</b>
          <div class="muted small">
            {#if c.title_id}<a class="link" href="#/title/{c.title_id}">{c.from}</a>{:else}{c.from}{/if}
          </div>
          <div class="faint small">From {clock(c.start)} for {clock(c.seconds)}{c.state === 'ready' ? ` · ${size(c)}` : ''} · {c.by ? `${c.by}, ` : ''}{ago(c.created_at)}</div>
          {#if c.said?.length}
            <details class="said">
              <summary class="small">What is said</summary>
              <div class="row wrap tools">
                <button class="btn small ghost" onclick={() => choose(c)}>{choosing === c.id ? 'Done choosing' : 'Choose lines for a picture'}</button>
                {#if choosing === c.id}
                  <button class="btn small ghost" onclick={() => (chosen = c.said!.map((_, i) => i))}>All</button>
                  <button class="btn small ghost" disabled={!chosen.length} onclick={() => (chosen = [])}>None</button>
                {/if}
              </div>
              <ol>
                {#each c.said as line, i}
                  <li>
                    {#if choosing === c.id}
                      <label class="line pick" class:on={chosen.includes(i)}>
                        <input type="checkbox" checked={chosen.includes(i)} onchange={() => pick(i)} />
                        <span>{line.text}</span>
                      </label>
                    {:else}
                      <button class="line" disabled={c.state !== 'ready'} onclick={() => playFrom(c, line.at)}>
                        <span class="at faint">{clock(line.at)}</span>
                        <span>{line.text}</span>
                      </button>
                    {/if}
                  </li>
                {/each}
              </ol>
              {#if choosing === c.id}
                {#if chosen.length}
                  <canvas class="card-preview" bind:this={card} aria-label="The picture of the chosen lines"></canvas>
                  <div class="row wrap tools">
                    {#if ready[c.id]?.type === 'image/png'}
                      <button class="btn small primary" onclick={() => share(c, ready[c.id])}>Save the picture now</button>
                    {:else}
                      <button class="btn small primary" onclick={() => saveCard(c)}>Save as picture</button>
                    {/if}
                  </div>
                {:else}
                  <div class="faint small">Tick the lines to put in the picture.</div>
                {/if}
              {/if}
            </details>
          {:else if c.said}
            <div class="faint small">Nothing is said in it.</div>
          {/if}
        </div>
        <div class="row wrap actions">
          {#if c.state === 'ready'}
            {#if saving[c.id]}
              <span class="small muted">{saving[c.id]}</span>
            {:else if ready[c.id]}
              <button class="btn small primary" onclick={() => share(c, ready[c.id])}>Save {ready[c.id].type === 'image/gif' ? 'the GIF' : 'it'} now</button>
            {:else}
              <button class="btn small" onclick={() => save(c, 'mp4')}>Save</button>
              <button class="btn small ghost" onclick={() => save(c, 'gif')}>Save as GIF</button>
            {/if}
            <button class="btn small ghost" onclick={() => copyLink(c)}>Copy link</button>
          {/if}
          {#if c.yours}
            {#if confirming === c.id}
              <span class="small muted">Remove it?</span>
              <button class="btn small danger" onclick={() => remove(c)}>Remove</button>
              <button class="btn small ghost" onclick={() => (confirming = null)}>Keep</button>
            {:else}
              <button class="btn small ghost danger" onclick={() => (confirming = c.id)}>{c.state === 'making' ? 'Cancel' : 'Remove'}</button>
            {/if}
          {/if}
        </div>
      </article>
    {/each}
  </div>
{/if}

<style>
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 14px;
    margin-bottom: 8px;
  }
  .lede {
    margin: 0 0 18px;
    max-width: 70ch;
  }
  .clips {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(100%, 320px), 1fr));
    gap: 16px;
  }
  .clip {
    padding: 0;
    overflow: hidden;
    display: flex;
    flex-direction: column;
  }
  .frame {
    aspect-ratio: 16 / 9;
    background: #000;
  }
  .frame video {
    display: block;
    width: 100%;
    height: 100%;
    object-fit: contain;
    background: #000;
  }
  .placeholder {
    height: 100%;
    display: flex;
    flex-direction: column;
    justify-content: center;
    gap: 10px;
    padding: 0 20px;
    color: #ddd;
    font-size: 13px;
  }
  .placeholder.bad {
    color: #f3b3ad;
  }
  .about {
    padding: 12px 14px 4px;
    display: grid;
    gap: 3px;
    min-width: 0;
  }
  .name {
    overflow-wrap: anywhere;
  }
  .actions {
    padding: 8px 14px 14px;
    margin-top: auto;
  }
  .said {
    margin-top: 6px;
  }
  .said summary {
    cursor: pointer;
    color: var(--muted);
  }
  .said ol {
    list-style: none;
    margin: 6px 0 0;
    padding: 0;
    max-height: 220px;
    overflow-y: auto;
  }
  .line {
    display: grid;
    grid-template-columns: 3.2em 1fr;
    gap: 8px;
    width: 100%;
    padding: 3px 0;
    background: none;
    border: 0;
    color: inherit;
    font: inherit;
    font-size: 13px;
    text-align: left;
    cursor: pointer;
  }
  .line:disabled {
    cursor: default;
  }
  .line:not(:disabled):hover span:last-child {
    text-decoration: underline;
  }
  .at {
    font-variant-numeric: tabular-nums;
  }
  .tools {
    margin: 6px 0 2px;
    gap: 6px;
  }
  .pick {
    grid-template-columns: 1.6em 1fr;
    align-items: start;
  }
  .pick input {
    margin: 2px 0 0;
  }
  .pick.on span {
    color: var(--text);
  }
  .card-preview {
    display: block;
    width: 100%;
    height: auto;
    margin-top: 8px;
    border-radius: 6px;
  }
</style>
