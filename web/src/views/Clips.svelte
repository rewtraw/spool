<script lang="ts">
  import { api, type Clip } from '../lib/api';
  import { app, act, toast } from '../lib/state.svelte';
  import { ago, bytes, clock, pct } from '../lib/format';
  import MomentFinder from '../components/MomentFinder.svelte';

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

  /** Save a clip, as its video or as a GIF: to Photos through the share sheet where there is
   * one, and as a download where there is not. */
  async function save(c: Clip, kind: 'mp4' | 'gif') {
    const url = kind === 'gif' ? `/api/clips/${c.id}/gif` : `/api/clips/${c.id}/video`;
    const sheet = typeof navigator.canShare === 'function';
    if (!sheet && kind === 'mp4') {
      location.href = `${url}?download`;
      return;
    }
    saving[c.id] = kind === 'gif' ? 'Making the GIF…' : 'Fetching…';
    try {
      const res = await fetch(url);
      if (!res.ok) throw new Error((await res.json().catch(() => null))?.error ?? `It could not be fetched (${res.status})`);
      const file = new File([await res.blob()], fileName(c, kind), { type: kind === 'gif' ? 'image/gif' : 'video/mp4' });
      if (sheet && navigator.canShare({ files: [file] })) {
        await share(c, file);
      } else {
        const a = document.createElement('a');
        a.href = URL.createObjectURL(file);
        a.download = file.name;
        a.click();
        setTimeout(() => URL.revokeObjectURL(a.href), 60000);
      }
    } catch (err: any) {
      toast(err.message, 'error');
    } finally {
      delete saving[c.id];
    }
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
            <video src="/api/clips/{c.id}/video" poster="/api/clips/{c.id}/still" controls playsinline preload="none"></video>
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
</style>
