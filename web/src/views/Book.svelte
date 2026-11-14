<script lang="ts">
  // One book: where it stands, the files the index offers for it, and what can be done.
  // Spool finds an EPUB of it in its index of Anna's Archive, downloads it, checks it and
  // hands it to Florilegium.
  import { api, type Book, type Candidate } from '../lib/api';
  import { act, go } from '../lib/state.svelte';
  import { ago, bytes, clock } from '../lib/format';
  import { player, playBook, chapterAt } from '../lib/player.svelte';

  let { id }: { id: number } = $props();
  let b = $state<Book | null>(null);
  let failed = $state(false);
  let florilegium = $state(false);
  let showFiles = $state(false);
  let md5 = $state('');
  let noCover = $state(false);

  async function load() {
    try {
      b = await api.get<Book>(`/books/${id}`);
    } catch {
      failed = true;
    }
  }
  load();
  // Where whoever is signed in left off in the audiobook.
  let kept = $state(0);
  // The page is made anew for each book (see App), so this is asked once.
  const readKept = () => api.get<{ position: number }>(`/books/${id}/listen`).then((r) => (kept = r.position), () => {});
  readKept();
  api.get<{ status: any }>('/books').then((r) => (florilegium = !!r.status?.florilegium)).catch(() => {});

  // While it is on its way, look again every few seconds.
  $effect(() => {
    const audioMoving = b?.audio && b.audio.state !== 'have';
    if (!b || (!(b.epub && ['wanted', 'fetching', 'sending'].includes(b.state)) && !audioMoving)) return;
    const t = setInterval(load, 4000);
    return () => clearInterval(t);
  });

  async function choose(hash: string) {
    if (await act(() => api.post(`/books/${id}/choose`, { md5: hash }), 'Downloading it')) {
      md5 = '';
      load();
    }
  }
  async function retry() {
    if (await act(() => api.post(`/books/${id}/retry`), 'Looking again')) load();
  }
  async function resend() {
    if (await act(() => api.post(`/books/${id}/send`), 'Sending it to Florilegium')) load();
  }
  async function remove() {
    if (!b) return;
    const file = b.file ? ` The EPUB Spool kept is deleted too${b.receipt ? '; Florilegium keeps its copy' : ''}.` : '';
    if (!confirm(`Remove “${b.title}” from Spool?${file}`)) return;
    if (await act(() => api.del(`/books/${id}?delete_file=true`), 'Removed')) go('library?kind=books');
  }

  const LABEL: Record<string, { text: string; tone: string }> = {
    wanted: { text: 'Wanted', tone: 'warn' },
    choose: { text: 'Choose a file', tone: 'accent' },
    fetching: { text: 'Downloading', tone: 'accent' },
    sending: { text: 'Sending', tone: 'accent' },
    have: { text: 'Got it', tone: 'ok' },
  };
  const sure = (c: Candidate) => `${Math.round(c.score * 100)}%`;

  async function wantAudio() {
    if (await act(() => api.post(`/books/${id}/audio`), 'Looking for the audiobook')) load();
  }
  async function wantEpub() {
    if (b && (await act(() => api.post('/books', { ol_work: b!.ol_work, formats: ['epub'] }), 'Looking for the EPUB'))) load();
  }
  async function audioAgain() {
    if (await act(() => api.post(`/books/${id}/audio/search`), 'Looking for it')) load();
  }
  async function dropAudio() {
    if (await act(() => api.del(`/books/${id}/audio`), 'Stopped wanting the audiobook')) load();
  }
  const AUDIO: Record<string, { text: string; tone: string }> = { wanted: { text: 'Wanted', tone: 'warn' }, downloading: { text: 'Downloading', tone: 'accent' }, have: { text: 'Here', tone: 'ok' } };
  const hours = (s: number) => (s >= 3600 ? `${Math.floor(s / 3600)} h ${Math.round((s % 3600) / 60)} min` : `${Math.round(s / 60)} min`);
  const listening = $derived(player.book?.id === id);
</script>

<p class="back"><a class="link" href="#/library?kind=books">← Books</a></p>

{#if failed}
  <div class="card empty">This book is not in Spool any more.</div>
{:else if !b}
  <div class="empty">Loading…</div>
{:else}
  {@const l = LABEL[b.state]}
  <div class="top">
    <div class="cover">{#if b.cover && !noCover}<img src={b.cover} alt="" onerror={() => (noCover = true)} />{:else}<span>{b.title}</span>{/if}</div>
    <div class="about">
      <div class="row wrap"><h1>{b.title}</h1>{#if b.epub}<span class="chip {l?.tone}">EPUB · {l?.text ?? b.state}</span>{/if}{#if b.audio}<span class="chip {AUDIO[b.audio.state].tone}">Audiobook · {AUDIO[b.audio.state].text}</span>{/if}</div>
      {#if b.subtitle}<div class="muted">{b.subtitle}</div>{/if}
      <div class="muted">{b.authors.join(', ')}{b.year ? ` · ${b.year}` : ''}</div>
      {#if b.epub && b.note}<div class="note">{b.note}</div>{/if}
      {#if b.receipt}<div class="faint small">In Florilegium since {ago(b.receipt.at)}{b.receipt.status === 'exists' ? ' (it had this file already)' : ''}.</div>{/if}
      {#if b.chosen && b.state !== 'have'}<div class="faint small">File: {b.chosen.title}{b.chosen.size ? ` · ${bytes(b.chosen.size)}` : ''}</div>{/if}
      <div class="row wrap actions">
        {#if b.audio?.state === 'have'}<button class="btn small primary" onclick={() => b && playBook(b)}>{kept > 10 ? `Listen from ${clock(kept)}` : 'Listen'}</button>{/if}
        {#if !b.audio}<button class="btn small" onclick={wantAudio}>Want the audiobook</button>{/if}
        {#if !b.epub}<button class="btn small" onclick={wantEpub}>Want the EPUB</button>{/if}
        {#if b.epub && (b.state === 'wanted' || b.state === 'choose')}<button class="btn small" onclick={retry}>Look again</button>{/if}
        {#if b.file}<a class="btn small" href="/api/books/{b.id}/file">Download EPUB</a>{/if}
        {#if b.file && florilegium && b.state !== 'sending'}<button class="btn small" onclick={resend}>{b.receipt ? 'Send again' : 'Send to Florilegium'}</button>{/if}
        {#if b.epub && b.state !== 'choose'}<button class="btn small ghost" onclick={() => (showFiles = !showFiles)}>{showFiles ? 'Hide files' : 'Files'}</button>{/if}
        <button class="btn small ghost danger" onclick={remove}>Remove</button>
      </div>
    </div>
  </div>

  {#if b.description}<p class="description muted">{b.description}</p>{/if}

  {#if b.audio}
    {@const a = b.audio}
    <div class="section-title">Audiobook</div>
    <div class="card audio">
      {#if a.state === 'have'}
        {@const parts = a.chapters.every((c) => /^Part \d+$/.test(c.title))}
        <div class="head faint small" title={a.folder}>
          {hours(a.seconds)} · {a.chapters.length} {parts ? (a.chapters.length === 1 ? 'part' : 'parts') : a.chapters.length === 1 ? 'chapter' : 'chapters'} · {(a.files[0]?.path.split('.').pop() ?? (a.kind === 'm4b' ? 'm4b' : '')).toUpperCase()}
        </div>
        <ol class="chapters">
          {#each a.chapters as c, i}
            <li class:now={listening && chapterAt(player.now) === i}>
              <button class="chapter" onclick={() => b && playBook(b, c.start)}>
                <span class="grow clamp">{c.title}</span>
                <span class="faint small mono">{clock(c.start)}</span>
              </button>
            </li>
          {/each}
        </ol>
      {:else}
        {#if a.note}<div class="note">{a.note}</div>{/if}
        {#if a.release}<div class="faint small mono">{a.release.title} · {a.release.indexer}{a.release.size ? ` · ${bytes(a.release.size)}` : ''}</div>{/if}
        <div class="row wrap actions">
          {#if a.state === 'wanted'}<button class="btn small" onclick={audioAgain}>Look now</button>{/if}
          {#if a.state === 'downloading'}<a class="btn small" href="#/activity">Activity</a>{/if}
          <button class="btn small ghost danger" onclick={dropAudio}>Stop wanting the audiobook</button>
        </div>
        {#if a.turned_down.length}
          <div class="faint small">Passed over: {#each a.turned_down as t, i}{i ? '; ' : ''}{t.why}{/each}</div>
        {/if}
      {/if}
    </div>
  {/if}

  {#if b.state === 'choose' || showFiles}
    <div class="section-title">Files in the index</div>
    <div class="card files">
      {#each b.candidates as c (c.md5)}
        <div class="file">
          <div class="grow">
            <div class="clamp">{c.title}</div>
            <div class="faint small">{c.author || 'No author given'} · {c.lang || '?'} · {c.year || '?'} · {bytes(c.size)} · {sure(c)} sure{c.by_id ? ' · ISBN matches' : ''}</div>
          </div>
          <button class="btn small" onclick={() => choose(c.md5)}>Use this</button>
        </div>
      {:else}
        <p class="faint small">The index offers no files for it.</p>
      {/each}
      <form class="row" onsubmit={(e) => { e.preventDefault(); choose(md5); }}>
        <input class="input mono grow" placeholder="Or an MD5 from Anna's Archive" bind:value={md5} aria-label="MD5 of a file" />
        <button class="btn small" disabled={md5.trim().length !== 32}>Use it</button>
      </form>
    </div>
    {#if b.turned_down.length}
      <div class="section-title">Turned down</div>
      <div class="card files">
        {#each b.turned_down as t}<div class="faint small">{t.why} · {ago(t.at)}</div>{/each}
      </div>
    {/if}
  {/if}
{/if}

<style>
  .back {
    margin: 0 0 14px;
  }
  .top {
    display: flex;
    gap: 20px;
    align-items: flex-start;
    flex-wrap: wrap;
  }
  .cover {
    width: 160px;
    max-width: 38vw;
    aspect-ratio: 2 / 3;
    border-radius: 8px;
    overflow: hidden;
    background: var(--raised);
    border: 1px solid var(--line);
    display: grid;
    place-items: center;
  }
  .cover img {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
  .cover span {
    padding: 10px;
    font-family: var(--serif);
    text-align: center;
  }
  .about {
    display: grid;
    gap: 6px;
    flex: 1 1 280px;
    min-width: 0;
  }
  .about h1 {
    margin: 0;
  }
  .actions {
    margin-top: 8px;
    gap: 6px;
  }
  .audio {
    display: grid;
    gap: 8px;
  }
  .audio:has(.chapters) {
    gap: 0;
    overflow: hidden;
  }
  .audio:not(:has(.chapters)) {
    padding: 12px 14px;
  }
  .head {
    padding: 10px 14px;
  }
  .chapters {
    list-style: none;
    padding: 0;
    margin: 0;
    max-height: 420px;
    overflow-y: auto;
  }
  .chapter {
    display: flex;
    align-items: baseline;
    gap: 10px;
    width: 100%;
    padding: 9px 14px;
    border: 0;
    border-top: 1px solid var(--line);
    background: none;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  li.now .chapter {
    color: var(--accent);
    font-weight: 600;
  }
  .description {
    margin: 20px 0 0;
    max-width: 70ch;
  }
  .section-title {
    margin: 24px 0 10px;
  }
  .files {
    display: grid;
    gap: 10px;
  }
  .file {
    display: flex;
    gap: 10px;
    align-items: center;
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
