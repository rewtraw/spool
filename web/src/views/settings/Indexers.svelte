<script lang="ts">
  // Newznab indexers. One needs its address and key; what it searches and how much it may be
  // asked are under More, set to what suits nearly every indexer.
  import { api } from '../../lib/api';
  import { act, loadStatus } from '../../lib/state.svelte';

  let indexers = $state<any[]>([]);
  let tests = $state<Record<string, { ok: boolean; message: string } | 'running'>>({});
  api.get('/indexers').then((r) => (indexers = r)).catch(() => {});

  function addIndexer() {
    indexers.push({ id: 0, name: '', url: '', api_key: '', movie_categories: [2000, 2010, 2020, 2030, 2040, 2045, 2050, 2060], tv_categories: [5030, 5040], enable_rss: true, enable_search: true, priority: 25, movies: true, tv: true });
  }
  const cats = (v: any) => (Array.isArray(v) ? v : String(v).split(/[,\s]+/).map(Number).filter((n) => n > 0));
  // Named after its address when not given a name.
  const nameOf = (ix: any) => ix.name?.trim() || (() => { try { return new URL(ix.url).hostname.replace(/^(api|www)\./, ''); } catch { return ''; } })();
  async function saveIndexer(ix: any) {
    const body = { ...ix, name: nameOf(ix), priority: Number(ix.priority), daily_requests: Number(ix.daily_requests) || 0, daily_grabs: Number(ix.daily_grabs) || 0, movie_categories: cats(ix.movie_categories), tv_categories: cats(ix.tv_categories) };
    const saved = await act(() => (ix.id ? api.put(`/indexers/${ix.id}`, body) : api.post('/indexers', body)), 'Saved');
    if (saved) indexers = await api.get('/indexers');
    loadStatus();
  }
  async function testIndexer(ix: any) {
    tests[`i${ix.id}`] = 'running';
    tests[`i${ix.id}`] = await api.post(`/indexers/${ix.id}/test`).catch((e) => ({ ok: false, message: e.message }));
  }
  async function removeIndexer(ix: any, i: number) {
    if (ix.id && !confirm(`Remove ${ix.name}?`)) return;
    if (ix.id) await act(() => api.del(`/indexers/${ix.id}`));
    indexers.splice(i, 1);
  }
</script>

<fieldset class="card">
  <legend>Indexers</legend>
  {#each indexers as ix, i}
    <div class="place">
      <div class="row wrap">
        <b>{nameOf(ix) || 'A new indexer'}</b>
        {#if ix.usage}<span class="small faint">today {ix.usage.requests}{ix.usage.request_limit ? ` of ${ix.usage.request_limit}` : ''} searches · {ix.usage.grabs}{ix.usage.grab_limit ? ` of ${ix.usage.grab_limit}` : ''} downloads</span>{/if}
        <span class="grow"></span>
        {#if tests[`i${ix.id}`]}<span class="result" class:bad={tests[`i${ix.id}`] !== 'running' && !(tests[`i${ix.id}`] as any).ok}>{tests[`i${ix.id}`] === 'running' ? 'Testing…' : (tests[`i${ix.id}`] as any).message}</span>{/if}
        {#if ix.id}<button class="btn small" type="button" onclick={() => testIndexer(ix)}>Test</button>{/if}
      </div>
      {#if ix.failing}<div class="small warn">Not asked for another {ix.failing.retry_in_minutes} min after an error: {ix.failing.error}</div>{/if}
      <div class="cols">
        <label class="field">Address<input class="input mono" bind:value={ix.url} placeholder="https://api.example.com" /></label>
        <label class="field">API key<input class="input mono" bind:value={ix.api_key} autocomplete="off" /></label>
      </div>
      <details>
        <summary class="small">More</summary>
        <div class="cols">
          <label class="field">Name<input class="input" bind:value={ix.name} placeholder={nameOf(ix)} /></label>
          <label class="field">Order<input class="input" type="number" bind:value={ix.priority} /><span class="hint">Lower wins a tie between equal releases.</span></label>
        </div>
        <div class="cols">
          <label class="field">Film categories<input class="input mono" value={ix.movie_categories} onchange={(e) => (ix.movie_categories = cats(e.currentTarget.value))} /></label>
          <label class="field">Series categories<input class="input mono" value={ix.tv_categories} onchange={(e) => (ix.tv_categories = cats(e.currentTarget.value))} /></label>
        </div>
        <div class="cols">
          <label class="field">Searches allowed a day<input class="input" type="number" min="0" bind:value={ix.daily_requests} placeholder="0" /><span class="hint">0 goes by what the indexer says, or no limit.</span></label>
          <label class="field">Downloads allowed a day<input class="input" type="number" min="0" bind:value={ix.daily_grabs} placeholder="0" /></label>
        </div>
        <div class="row wrap">
          <label class="check"><input type="checkbox" bind:checked={ix.movies} /> Films</label>
          <label class="check"><input type="checkbox" bind:checked={ix.tv} /> Series</label>
          <label class="check"><input type="checkbox" bind:checked={ix.enable_rss} /> New releases</label>
          <label class="check"><input type="checkbox" bind:checked={ix.enable_search} /> Searches</label>
          <span class="grow"></span>
          <button class="btn small ghost danger" type="button" onclick={() => removeIndexer(ix, i)}>Remove</button>
        </div>
      </details>
      <div><button class="btn small primary" type="button" onclick={() => saveIndexer(ix)}>Save</button></div>
    </div>
  {/each}
  {#if !indexers.length}<p class="small warn">No indexer yet. Spool cannot find releases without one.</p>{/if}
  <div><button class="btn" type="button" onclick={addIndexer}>Add an indexer</button></div>
</fieldset>
