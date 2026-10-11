<script lang="ts">
  // The Indexers tab.
  import { api } from '../../lib/api';
  import { act, loadStatus } from '../../lib/state.svelte';

  let indexers = $state<any[]>([]);
  let tests = $state<Record<string, { ok: boolean; message: string } | 'running'>>({});
  api.get('/indexers').then((r) => (indexers = r)).catch(() => {});

  // ---- indexers
  function addIndexer() {
    indexers.push({ id: 0, name: '', url: '', api_key: '', movie_categories: [2000, 2010, 2020, 2030, 2040, 2045, 2050, 2060], tv_categories: [5030, 5040], enable_rss: true, enable_search: true, priority: 25, movies: true, tv: true });
  }
  const cats = (v: any) => (Array.isArray(v) ? v : String(v).split(/[,\s]+/).map(Number).filter((n) => n > 0));
  async function saveIndexer(ix: any, i: number) {
    const body = { ...ix, priority: Number(ix.priority), daily_requests: Number(ix.daily_requests) || 0, daily_grabs: Number(ix.daily_grabs) || 0, movie_categories: cats(ix.movie_categories), tv_categories: cats(ix.tv_categories) };
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

  <div class="stack form">
    {#each indexers as ix, i}
      <fieldset class="card">
        <legend>{ix.name || 'New indexer'}</legend>
        <div class="cols">
          <label class="field">Name<input class="input" bind:value={ix.name} /></label>
          <label class="field">Address<input class="input mono" bind:value={ix.url} placeholder="https://api.example.com" /></label>
          <label class="field">API key<input class="input mono" bind:value={ix.api_key} autocomplete="off" /></label>
        </div>
        <div class="cols">
          <label class="field">Movie categories<input class="input mono" value={ix.movie_categories} onchange={(e) => (ix.movie_categories = cats(e.currentTarget.value))} /></label>
          <label class="field">TV categories<input class="input mono" value={ix.tv_categories} onchange={(e) => (ix.tv_categories = cats(e.currentTarget.value))} /></label>
          <label class="field">Priority<input class="input" type="number" bind:value={ix.priority} /><span class="hint">Lower wins a tie between equal releases.</span></label>
        </div>
        <div class="cols">
          <label class="field">Requests allowed per day<input class="input" type="number" min="0" bind:value={ix.daily_requests} placeholder="0" /><span class="hint">0 goes by what the indexer reports, or no limit.</span></label>
          <label class="field">Downloads allowed per day<input class="input" type="number" min="0" bind:value={ix.daily_grabs} placeholder="0" /></label>
          <div class="field">Today
            {#if ix.usage}
              <div class="usage">
                <b>{ix.usage.requests}</b>{ix.usage.request_limit ? ` of ${ix.usage.request_limit}` : ''} requests · <b>{ix.usage.grabs}</b>{ix.usage.grab_limit ? ` of ${ix.usage.grab_limit}` : ''} downloads
              </div>
              <span class="hint">{ix.usage.source === 'reported' ? 'Limits as reported by the indexer.' : ix.usage.source === 'set' ? 'Limits as set here.' : ix.usage.source === 'reported_unlimited' ? 'The indexer reports usage and no limit for this account.' : 'The indexer does not report an allowance.'}</span>
            {:else}<div class="usage faint">Nothing yet</div>{/if}
          </div>
        </div>
        {#if ix.failing}<div class="notice warn">Not being asked for another {ix.failing.retry_in_minutes} min after an error: {ix.failing.error}</div>{/if}
        <div class="row wrap">
          <label class="check"><input type="checkbox" bind:checked={ix.movies} /> Movies</label>
          <label class="check"><input type="checkbox" bind:checked={ix.tv} /> Series</label>
          <label class="check"><input type="checkbox" bind:checked={ix.enable_rss} /> Feeds</label>
          <label class="check"><input type="checkbox" bind:checked={ix.enable_search} /> Searches</label>
          <span class="grow"></span>
          {#if tests[`i${ix.id}`]}<span class="result" class:bad={tests[`i${ix.id}`] !== 'running' && !(tests[`i${ix.id}`] as any).ok}>{tests[`i${ix.id}`] === 'running' ? 'Testing…' : (tests[`i${ix.id}`] as any).message}</span>{/if}
          {#if ix.id}<button class="btn small" onclick={() => testIndexer(ix)}>Test</button>{/if}
          <button class="btn small primary" onclick={() => saveIndexer(ix, i)}>Save</button>
          <button class="btn small ghost danger" onclick={() => removeIndexer(ix, i)}>Remove</button>
        </div>
      </fieldset>
    {/each}
    {#if !indexers.length}<div class="card empty">No indexer yet. Spool cannot find releases without one.</div>{/if}
    <div><button class="btn" onclick={addIndexer}>Add an indexer</button></div>
  </div>
