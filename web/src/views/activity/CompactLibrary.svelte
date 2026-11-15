<script lang="ts">
  // How the library stands for compact versions: what has one, what is being made, and what
  // would gain from one, with buttons to make them. How they are made is in Settings.
  import { api } from '../../lib/api';
  import { act, toast } from '../../lib/state.svelte';
  import { bytes, duration } from '../../lib/format';

  let report = $state<any>(null);
  let asking = $state(false);
  let some = $state(10);
  const making = $derived(report?.making?.now?.length > 0 || report?.making?.waiting_count > 0);
  async function load() {
    report = await api.get<any>('/compact').catch(() => report);
  }
  load();
  $effect(() => {
    if (!making) return;
    const timer = setInterval(load, 5000);
    return () => clearInterval(timer);
  });
  async function batch() {
    const n = Math.min(Math.max(1, Math.floor(Number(some)) || 0), report?.would?.files ?? 0);
    if (!n) return;
    asking = true;
    const r = await act(() => api.post<any>('/compact/everything', { count: n }));
    asking = false;
    if (r) toast(`${r.queued} in line. Progress is on the Queue.`);
    load();
  }
  async function everything() {
    const w = report?.would;
    if (!w?.files) return;
    if (!confirm(`Make ${w.files} compact ${w.files === 1 ? 'version' : 'versions'}?\n\nThey will take about ${bytes(w.bytes)} and are made one at a time in the background. The originals are not touched. The line can be cancelled at any time.`)) return;
    asking = true;
    const r = await act(() => api.post<any>('/compact/everything'));
    asking = false;
    if (r) toast(`${r.queued} in line. Progress is on the Queue.`);
    load();
  }
  async function cancelAll() {
    await act(() => api.post('/compact/cancel', {}), 'The waiting compact versions are cancelled');
    load();
  }
</script>

<fieldset class="card">
  <legend>Compact versions</legend>
  {#if !report}
    <p class="muted">Working it out…</p>
  {:else}
    <p><b>{report.made.files}</b> compact {report.made.files === 1 ? 'version' : 'versions'}{#if report.made.files}, {bytes(report.made.bytes)} beside {bytes(report.made.original_bytes)} of originals{/if}.</p>
    {#if making}
      <p class="muted">{report.making.now.length ? `Making ${report.making.now.map((n: any) => `${n.title}${n.episode ? ` ${n.episode}` : ''}${n.by ? ` on ${n.by}` : ''}`).join(', ')}` : 'Starting'}{report.making.waiting_count ? `, with ${report.making.waiting_count} waiting` : ''}.</p>
      {#if report.making.waiting_count}<div><button class="btn small ghost" type="button" onclick={cancelAll}>Cancel all waiting</button></div>{/if}
    {/if}
    {#if report.would.files}
      <p><b>{report.would.files}</b> more {report.would.files === 1 ? 'file' : 'files'} would gain from one: about {bytes(report.would.bytes)} from {bytes(report.would.original_bytes)}, and {duration(report.would.running_secs)} of film to get through. <a class="link" href="#/settings/preferences">How they are made</a></p>
      <div class="row wrap">
        {#if report.would.files > 1}
          <input class="input some" type="number" min="1" max={report.would.files} step="1" aria-label="How many to make" bind:value={some} />
          <button class="btn" type="button" disabled={asking} onclick={batch}>Make that many, biggest first</button>
        {/if}
        <button class="btn" class:ghost={report.would.files > 1} type="button" disabled={asking} onclick={everything}>{asking ? 'Putting them in line…' : `Make all ${report.would.files}`}</button>
      </div>
    {:else if !making}
      <p class="muted">Nothing else in the library would gain from one.</p>
    {/if}
    {#if Object.keys(report.why_not ?? {}).length}
      <details>
        <summary class="small">Files that get none, and why</summary>
        <div class="list">
          {#each Object.entries(report.why_not) as [why, n]}<div class="item small"><b>{n}</b> <span class="muted">{why}</span></div>{/each}
        </div>
      </details>
    {/if}
  {/if}
</fieldset>

<style>
  .some {
    width: 5.5em;
  }
</style>
