<script lang="ts">
  // The Books tab: the book index and today's downloads, the Anna's Archive key books are
  // downloaded with, where they go, and the languages they are read in.
  import { api } from '../../lib/api';
  import { act, toast } from '../../lib/state.svelte';
  import { ago } from '../../lib/format';

  let status = $state<any>(null);
  // A build asked for, until the server says it has begun or has stopped: it first fetches
  // Anna's Archive's list of snapshots, which takes a while.
  let starting = $state(0);
  async function loadStatus() {
    status = (await api.get<{ status: any }>('/books').catch(() => null))?.status ?? status;
  }
  loadStatus();
  const index = $derived(status?.index);
  $effect(() => {
    if (starting && (index?.building || index?.error || Date.now() - starting > 5 * 60_000)) starting = 0;
  });
  $effect(() => {
    if (!index?.building && !starting) return;
    const t = setInterval(loadStatus, 4000);
    return () => clearInterval(t);
  });
  async function buildIndex() {
    const r = await act(() => api.post<any>('/books/index'));
    if (r) toast(r.started ? 'Building the book index' : r.message);
    if (r?.started) {
      if (status?.index) status.index.error = '';
      starting = Date.now();
    }
    loadStatus();
  }
  const pct = (n: number, d: number) => (d ? Math.min(100, Math.round((100 * n) / d)) : 0);

  let general = $state<any>(null);
  async function load() {
    general = await api.get('/settings/general');
  }
  load().catch(() => {});
  async function save(e: Event) {
    e.preventDefault();
    await act(() => api.put('/settings/general', general), 'Saved');
    load();
  }
</script>

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
        <b>No book index yet.</b> <span class="muted">{status.has_key ? 'Building it reads about 170 GB of Anna’s Archive’s metadata, one part at a time, and keeps a few hundred megabytes.' : 'Add an Anna’s Archive key below first.'}</span>
      {/if}
      {#if index?.error && !index?.building}<div class="warn small">The last build stopped: {index.error}</div>{/if}
      <div class="faint small">
        {#if status.allowance}{status.allowance.downloads_left} of {status.allowance.downloads_per_day} downloads left today · {/if}
        {status.florilegium ? 'Books go to Florilegium.' : 'Books are kept in Spool; Florilegium is not set up.'}
      </div>
    </div>
    {#if status.has_key && !index?.building && !starting}<button class="btn small" onclick={buildIndex}>{status.index_books != null ? 'Build again' : 'Build the index'}</button>{/if}
  </div>
{/if}

{#if general}
  <form class="stack form" onsubmit={save}>
    <fieldset class="card">
      <legend>Anna's Archive</legend>
      <p class="muted">Books are found in an index Spool builds from the metadata Anna's Archive publishes, and downloaded with a member's key. Each download is checked against the file it was asked for.</p>
      <label class="field">Secret key<input class="input mono" bind:value={general.books.annas_key} autocomplete="off" /><span class="hint">From your account page on Anna's Archive, once you are a member. Each membership allows a number of downloads a day.</span></label>
      <label class="field narrow">Languages<input class="input" bind:value={general.books.languages} placeholder="en" autocomplete="off" /><span class="hint">Two-letter codes, separated by commas: <code>en</code>, or <code>en, fr</code>. The index keeps books in these alone; build it again after changing them.</span></label>
    </fieldset>
    <fieldset class="card">
      <legend>Florilegium</legend>
      <p class="muted">Each book Spool gets is put on Florilegium's shelf and in its library, and marked as got on its reading list. Without it, books are kept in Spool to download.</p>
      <label class="field">Address<input class="input mono" bind:value={general.books.florilegium_url} placeholder="http://melchior:8790" autocomplete="off" /></label>
      <label class="field">Token<input class="input mono" type="password" bind:value={general.books.florilegium_token} autocomplete="new-password" /><span class="hint">The token Florilegium's server is started with.</span></label>
    </fieldset>
    <div><button class="btn primary">Save</button></div>
  </form>
{:else}
  <div class="empty">Loading…</div>
{/if}

<style>
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
  .grow {
    flex: 1;
    min-width: 0;
  }
  .warn {
    color: var(--warn);
  }
</style>
