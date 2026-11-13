<script lang="ts">
  // Books: find one in Open Library's catalogue and add it; Spool finds an EPUB of it in
  // its index of Anna's Archive, downloads it, checks it and hands it to Florilegium.
  import { api, type Book, type Candidate } from '../lib/api';
  import { act, toast } from '../lib/state.svelte';
  import { ago, bytes } from '../lib/format';

  let books = $state<Book[]>([]);
  let status = $state<any>(null);
  let loaded = $state(false);
  let query = $state('');
  let found = $state<any[]>([]);
  let searching = $state(false);
  let searched = $state('');
  let open = $state<Record<number, boolean>>({});
  let md5 = $state<Record<number, string>>({});
  // A build asked for, until the server says it has begun or has stopped: it first fetches
  // Anna's Archive's list of snapshots, which takes a while.
  let starting = $state(0);

  async function load() {
    const r = await api.get<{ books: Book[]; status: any }>('/books').catch(() => null);
    if (r) {
      books = r.books;
      status = r.status;
    }
    loaded = true;
  }
  load();

  // While anything is on its way, look again every few seconds.
  const moving = $derived(books.some((b) => b.state === 'fetching' || b.state === 'sending') || status?.index?.building || starting > 0);
  $effect(() => {
    if (starting && (status?.index?.building || status?.index?.error || Date.now() - starting > 5 * 60_000)) starting = 0;
  });
  $effect(() => {
    if (!moving) return;
    const t = setInterval(load, 4000);
    return () => clearInterval(t);
  });

  let seq = 0;
  async function search(e?: Event) {
    e?.preventDefault();
    const q = query.trim();
    if (q.length < 2) return;
    const mine = ++seq;
    searching = true;
    try {
      const r = await api.get<any[]>(`/books/lookup?q=${encodeURIComponent(q)}`);
      if (mine === seq) {
        found = r;
        searched = q;
      }
    } catch (err: any) {
      if (mine === seq) toast(err.message);
    } finally {
      if (mine === seq) searching = false;
    }
  }

  async function add(w: any) {
    const b = await act(() => api.post<Book>('/books', { ol_work: w.ol_work }), `Added ${w.title}`);
    if (b) {
      w.book_id = b.id;
      await load();
    }
  }

  async function choose(b: Book, hash: string) {
    if (await act(() => api.post(`/books/${b.id}/choose`, { md5: hash }), 'Downloading it')) {
      md5[b.id] = '';
      load();
    }
  }
  async function retry(b: Book) {
    if (await act(() => api.post(`/books/${b.id}/retry`), 'Looking again')) load();
  }
  async function resend(b: Book) {
    if (await act(() => api.post(`/books/${b.id}/send`), 'Sending it to Florilegium')) load();
  }
  async function remove(b: Book) {
    const file = b.file ? ` The EPUB Spool kept is deleted too${b.receipt ? '; Florilegium keeps its copy' : ''}.` : '';
    if (!confirm(`Remove “${b.title}” from Spool?${file}`)) return;
    if (await act(() => api.del(`/books/${b.id}?delete_file=true`), 'Removed')) load();
  }
  async function buildIndex() {
    const r = await act(() => api.post<any>('/books/index'));
    if (r) toast(r.started ? 'Building the book index' : r.message);
    if (r?.started) {
      if (status?.index) status.index.error = '';
      starting = Date.now();
    }
    load();
  }

  const label: Record<string, { text: string; tone: string }> = {
    wanted: { text: 'Wanted', tone: 'warn' },
    choose: { text: 'Choose a file', tone: 'accent' },
    fetching: { text: 'Downloading', tone: 'accent' },
    sending: { text: 'Sending', tone: 'accent' },
    have: { text: 'Got it', tone: 'ok' },
  };
  const by = (b: { authors?: string[] }) => (b.authors ?? []).join(', ');
  const pct = (n: number, d: number) => (d ? Math.min(100, Math.round((100 * n) / d)) : 0);
  const sure = (c: Candidate) => `${Math.round(c.score * 100)}%`;
  const index = $derived(status?.index);
</script>

<header class="head">
  <h1>Books</h1>
</header>

{#if status}
  <div class="card strip">
    <div class="grow">
      {#if starting && !index?.building}
        <b>Starting to build the book index…</b> <span class="muted">Fetching Anna’s Archive’s list of snapshots.</span>
      {:else if index?.building}
        <b>Building the book index</b>
        <span class="muted">· part {index.part} of {index.parts}{index.phase ? `, ${index.phase}` : ''} · {(index.kept ?? 0).toLocaleString()} books so far</span>
        <div class="progress"><i style="width: {pct(index.read_bytes, index.total_bytes)}%"></i></div>
      {:else if status.index_books != null}
        <b>{status.index_books.toLocaleString()}</b> <span class="muted">books in the index{index?.snapshot ? `, from Anna's Archive's ${index.snapshot.replace('aa_derived_mirror_metadata_', 'snapshot of ')}` : ''}{index?.built_at ? `, built ${ago(index.built_at)}` : ''}.</span>
      {:else}
        <b>No book index yet.</b> <span class="muted">{status.has_key ? 'Building it reads about 170 GB of Anna’s Archive’s metadata, one part at a time, and keeps a few hundred megabytes.' : 'Add an Anna’s Archive key in Settings → Books first.'}</span>
      {/if}
      {#if index?.error && !index?.building}<div class="warn small">The last build stopped: {index.error}</div>{/if}
      <div class="faint small">
        {#if status.allowance}{status.allowance.downloads_left} of {status.allowance.downloads_per_day} downloads left today · {/if}
        {status.florilegium ? 'Books go to Florilegium.' : 'Books are kept in Spool; Florilegium is not set up.'}
        <a class="link" href="#/settings/books">Settings</a>
      </div>
    </div>
    {#if status.has_key && !index?.building && !starting}<button class="btn small" onclick={buildIndex}>{status.index_books != null ? 'Build again' : 'Build the index'}</button>{/if}
  </div>
{/if}

<form class="bar" onsubmit={search}>
  <input class="input" type="search" placeholder="Find a book by title or author" bind:value={query} aria-label="Find a book" />
  <button class="btn primary" disabled={searching || query.trim().length < 2}>{searching ? 'Searching…' : 'Search'}</button>
</form>

{#if searched}
  {#if !found.length}
    <div class="card empty">Open Library has nothing for “{searched}”.</div>
  {:else}
    <p class="faint source">From Open Library. <button class="link" onclick={() => ((found = []), (searched = ''))}>Clear</button></p>
    <div class="found">
      {#each found as w (w.ol_work)}
        <div class="card work">
          {#if w.cover}<img src={w.cover} alt="" loading="lazy" />{:else}<div class="nocover">{w.title}</div>{/if}
          <div class="about">
            <b class="clamp">{w.title}</b>
            <span class="muted small clamp">{by(w)}</span>
            <span class="faint small">{w.year || ''}{w.editions ? ` · ${w.editions} editions` : ''}</span>
            {#if w.book_id}<span class="chip ok">Added</span>{:else}<button class="btn small primary" onclick={() => add(w)}>Add</button>{/if}
          </div>
        </div>
      {/each}
    </div>
  {/if}
{/if}

<div class="section-title">Your books</div>
{#if !loaded}
  <div class="empty">Loading…</div>
{:else if !books.length}
  <div class="card empty">No books yet. Find one above and add it.</div>
{:else}
  <div class="stack">
    {#each books as b (b.id)}
      {@const l = label[b.state]}
      <div class="card book">
        {#if b.cover}<img src={b.cover} alt="" loading="lazy" />{:else}<div class="nocover small">{b.title}</div>{/if}
        <div class="grow">
          <div class="row wrap">
            <b>{b.title}</b>
            <span class="chip {l?.tone}">{l?.text ?? b.state}</span>
          </div>
          <div class="muted small">{by(b)}{b.year ? ` · ${b.year}` : ''}</div>
          {#if b.note}<div class="small note">{b.note}</div>{/if}
          {#if b.receipt}<div class="faint small">In Florilegium since {ago(b.receipt.at)}{b.receipt.status === 'exists' ? ' (it had this file already)' : ''}.</div>{/if}
          {#if b.chosen && b.state !== 'have'}<div class="faint small">File: {b.chosen.title}{b.chosen.size ? ` · ${bytes(b.chosen.size)}` : ''}</div>{/if}

          {#if b.state === 'choose' || open[b.id]}
            <div class="files">
              {#each b.candidates as c (c.md5)}
                <div class="file">
                  <div class="grow">
                    <div class="clamp">{c.title}</div>
                    <div class="faint small">{c.author || 'No author given'} · {c.lang || '?'} · {c.year || '?'} · {bytes(c.size)} · {sure(c)} sure{c.by_id ? ' · ISBN matches' : ''}</div>
                  </div>
                  <button class="btn small" onclick={() => choose(b, c.md5)}>Use this</button>
                </div>
              {:else}
                <p class="faint small">The index offers no files for it.</p>
              {/each}
              <form class="row" onsubmit={(e) => { e.preventDefault(); choose(b, md5[b.id] ?? ''); }}>
                <input class="input mono small-input" placeholder="Or an MD5 from Anna's Archive" bind:value={md5[b.id]} aria-label="MD5 of a file" />
                <button class="btn small" disabled={(md5[b.id] ?? '').trim().length !== 32}>Use it</button>
              </form>
            </div>
          {/if}
          {#if b.turned_down.length && open[b.id]}
            <div class="faint small">Turned down: {#each b.turned_down as t, i}{i ? '; ' : ''}{t.why}{/each}</div>
          {/if}

          <div class="row wrap actions">
            {#if b.state !== 'choose'}<button class="btn small ghost" onclick={() => (open[b.id] = !open[b.id])}>{open[b.id] ? 'Hide files' : 'Files'}</button>{/if}
            {#if b.state === 'wanted' || b.state === 'choose'}<button class="btn small ghost" onclick={() => retry(b)}>Look again</button>{/if}
            {#if b.file}<a class="btn small ghost" href="/api/books/{b.id}/file">Download EPUB</a>{/if}
            {#if b.file && status?.florilegium && b.state !== 'sending'}<button class="btn small ghost" onclick={() => resend(b)}>{b.receipt ? 'Send again' : 'Send to Florilegium'}</button>{/if}
            <button class="btn small ghost" onclick={() => remove(b)}>Remove</button>
          </div>
        </div>
      </div>
    {/each}
  </div>
{/if}

<style>
  .head {
    margin-bottom: 18px;
  }
  .strip {
    display: flex;
    gap: 14px;
    align-items: center;
    padding: 14px 16px;
    margin-bottom: 18px;
  }
  .strip .progress {
    margin: 8px 0 4px;
  }
  .warn {
    color: var(--warn);
  }
  .bar {
    display: flex;
    gap: 10px;
    margin-bottom: 18px;
  }
  .bar .input {
    flex: 1 1 auto;
    width: auto;
    min-width: 0;
  }
  .source {
    margin: 0 0 10px;
  }
  .found {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(100%, 300px), 1fr));
    gap: 12px;
    margin-bottom: 26px;
  }
  .work,
  .book {
    display: flex;
    gap: 14px;
    padding: 12px;
    min-width: 0;
  }
  .work img,
  .work .nocover {
    width: 64px;
    height: 96px;
  }
  .book img,
  .book .nocover {
    width: 56px;
    height: 84px;
  }
  img,
  .nocover {
    flex: none;
    object-fit: cover;
    border-radius: 4px;
    background: var(--raised);
  }
  .nocover {
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 4px;
    text-align: center;
    font-family: var(--serif);
    font-size: 11px;
    color: var(--muted);
    overflow: hidden;
  }
  .about {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
    align-items: flex-start;
  }
  .about .btn,
  .about .chip {
    margin-top: auto;
  }
  .clamp {
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
    overflow-wrap: anywhere;
  }
  .note {
    margin-top: 4px;
  }
  .files {
    margin-top: 10px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .file {
    display: flex;
    gap: 10px;
    align-items: center;
    padding: 8px 10px;
    border: 1px solid var(--line);
    border-radius: 8px;
    min-width: 0;
  }
  .small-input {
    flex: 1 1 auto;
    width: auto;
    min-width: 0;
    font-size: 13px;
  }
  .actions {
    margin-top: 8px;
    gap: 6px;
  }
  .grow {
    min-width: 0;
  }
  button.link {
    background: none;
    border: 0;
    padding: 0;
    color: inherit;
    text-decoration: underline;
    cursor: pointer;
    font: inherit;
  }
</style>
