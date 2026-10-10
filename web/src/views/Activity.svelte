<script lang="ts">
  import { api } from '../lib/api';
  import { app, act, route, go } from '../lib/state.svelte';
  import { ago, bytes, duration, pct, qualityName, STATE_LABEL } from '../lib/format';
  import JobLine from '../components/JobLine.svelte';

  const tab = $derived(route.arg || 'queue');
  let data = $state<any>({ items: [], orphan_jobs: [], paused: false });
  let history = $state<any[]>([]);
  let blocklist = $state<any[]>([]);
  let loaded = $state(false);

  async function load() {
    if (tab === 'queue') data = await api.get('/activity').catch(() => data);
    if (tab === 'history') history = await api.get('/history?limit=300').catch(() => []);
    if (tab === 'blocklist') blocklist = await api.get('/blocklist').catch(() => []);
    loaded = true;
  }

  $effect(() => {
    app.tick;
    tab;
    load();
  });

  const live = (i: any) => app.jobs[i.acquisition.job_id] ?? i.job;
  const queue = $derived(data.items.filter((i: any) => ['downloading', 'importing', 'import_blocked'].includes(i.acquisition.state)));
  const recent = $derived(data.items.filter((i: any) => !['downloading', 'importing', 'import_blocked'].includes(i.acquisition.state)).slice(0, 25));
  const totalSpeed = $derived(Object.values(app.jobs).reduce((s, j) => s + (j.state === 'downloading' ? j.speed : 0), 0));
  // The whole queue at a glance: how much there is, how much has arrived, and when it will be done.
  const totals = $derived.by(() => {
    let total = 0, done = 0, waiting = 0, count = 0;
    // Downloads that finished during this run stay counted, so the bar fills instead of slipping back.
    let arrived = data.finished?.count ?? 0;
    total += data.finished?.bytes ?? 0;
    done += data.finished?.bytes ?? 0;
    for (const i of queue) {
      const j = live(i);
      const size = j?.total_bytes || i.acquisition.release.size || 0;
      if (i.acquisition.state !== 'downloading') {
        total += size;
        done += size;
        arrived++;
        continue;
      }
      const finishedDownloading = j && !['queued', 'downloading', 'paused'].includes(j.state);
      total += size;
      done += finishedDownloading ? size : Math.min(j?.done_bytes ?? 0, size);
      if (j?.state === 'paused') waiting++;
      count++;
    }
    const left = total - done;
    return { total, done, left, waiting, count, arrived, eta: totalSpeed > 0 && left > 0 ? left / totalSpeed : null };
  });

  async function action(id: number, a: string, msg?: string) {
    await act(() => api.post(`/activity/${id}/${a}`), msg);
    load();
  }
  async function toggleQueue() {
    const r = await act(() => api.post(`/queue/${data.paused ? 'resume' : 'pause'}`));
    if (r) data.paused = r.paused;
  }

  const HISTORY: Record<string, string> = {
    grabbed: 'Sent to download',
    would_grab: 'Would have grabbed',
    imported: 'Imported',
    upgraded: 'Upgraded',
    download_failed: 'Download failed',
    file_deleted: 'File deleted',
    file_missing: 'File went missing',
  };
  const tone = (k: string) => (k === 'download_failed' || k === 'file_missing' ? 'bad' : k === 'imported' || k === 'upgraded' ? 'ok' : k === 'would_grab' ? 'warn' : '');
</script>

<header class="head">
  <h1>Activity</h1>
  {#if tab === 'queue'}
    <div class="row">
      {#if totalSpeed > 0}<span class="muted speed">{bytes(totalSpeed)}/s</span>{/if}
      <button class="btn" onclick={toggleQueue}>{data.paused ? 'Resume downloads' : 'Pause downloads'}</button>
    </div>
  {/if}
</header>

<div class="tabs" role="tablist">
  {#each [['queue', 'Queue'], ['history', 'History'], ['blocklist', 'Blocklist']] as [id, label]}
    <a role="tab" aria-selected={tab === id} class:on={tab === id} href="#/activity/{id}">{label}</a>
  {/each}
</div>

{#if tab === 'queue'}
  {#if data.paused}<div class="notice">Downloads are paused.</div>{/if}
  {#if totals.count > 0}
    <div class="card summary">
      <div class="row wrap figures">
        <div><b>{bytes(totals.done)}</b> <span class="muted">of {bytes(totals.total)}</span></div>
        <div class="muted">{totals.count} {totals.count === 1 ? 'download' : 'downloads'} left{totals.arrived ? `, ${totals.arrived} finished` : ''}{totals.waiting ? `, ${totals.waiting} paused or waiting` : ''}</div>
        {#if totals.left > 0}<div class="muted">{bytes(totals.left)} to go{totals.eta != null ? ` · about ${duration(totals.eta)} at ${bytes(totalSpeed)}/s` : ''}</div>{/if}
        <span class="grow"></span>
        {#if app.status?.free_bytes != null}<div class="muted" class:low={app.status.free_bytes < totals.left}>{bytes(app.status.free_bytes)} free on disk</div>{/if}
      </div>
      <div class="progress" role="progressbar" aria-label="Queue progress" aria-valuenow={Math.round(pct(totals.done, totals.total))} aria-valuemin="0" aria-valuemax="100"><i style="width:{pct(totals.done, totals.total)}%"></i></div>
    </div>
  {/if}
  {#if !queue.length && loaded}
    <div class="card empty">Nothing is downloading.</div>
  {/if}
  <div class="stack">
    {#each queue as i (i.acquisition.id)}
      {@const a = i.acquisition}
      {@const j = live(i)}
      <div class="card item" class:blocked={a.state === 'import_blocked'}>
        <div class="grow">
          <div class="row wrap top">
            <a class="t" href="#/title/{a.title_id}">{i.title ?? 'Unknown title'}</a>
            <span class="chip {a.state === 'import_blocked' ? 'warn' : 'accent'}">{a.state === 'downloading' ? STATE_LABEL[j?.state ?? 'queued'] : STATE_LABEL[a.state]}</span>
            <span class="chip">{qualityName(a.quality, i.kind)}</span>
          </div>
          <div class="mono faint break rel">{a.release.title}</div>
          <JobLine job={j} state={a.state} error={a.error} size={a.release.size} />
          <div class="faint why">{a.release.indexer} · {a.reason}</div>
        </div>
        <div class="buttons">
          {#if a.state === 'downloading' && j && j.state !== 'paused'}<button class="btn small" onclick={() => action(a.id, 'pause')}>Pause</button>{/if}
          {#if j?.state === 'paused'}<button class="btn small" onclick={() => action(a.id, 'resume')}>Resume</button>{/if}
          {#if a.state === 'import_blocked'}<button class="btn small" onclick={() => action(a.id, 'retry_import', 'Trying the import again')}>Import anyway</button>{/if}
          <button class="btn small" onclick={() => action(a.id, 'cancel_blocklist', 'Removed. Looking for another release.')}>Remove and find another</button>
          <button class="btn small ghost danger" onclick={() => action(a.id, 'cancel', 'Removed')}>Remove</button>
        </div>
      </div>
    {/each}
    {#each data.orphan_jobs as j (j.id)}
      <div class="card item">
        <div class="grow">
          <div class="row wrap top"><span class="t">{j.name}</span><span class="chip">{STATE_LABEL[j.state] ?? j.state}</span></div>
          <JobLine job={app.jobs[j.id] ?? j} state="downloading" error={j.error} />
          <div class="faint why">Not linked to a title</div>
        </div>
        <div class="buttons"><button class="btn small ghost danger" onclick={async () => { await act(() => api.del(`/jobs/${j.id}`)); load(); }}>Remove</button></div>
      </div>
    {/each}
  </div>

  {#if recent.length}
    <div class="section-title" style="margin:28px 0 10px">Recently finished</div>
    <div class="card scroll-x">
      <table class="table">
        <tbody>
          {#each recent as i (i.acquisition.id)}
            {@const a = i.acquisition}
            <tr>
              <td style="white-space:nowrap"><span class="chip {a.state === 'imported' ? 'ok' : a.state === 'failed' ? 'bad' : ''}">{STATE_LABEL[a.state]}</span></td>
              <td><a href="#/title/{a.title_id}"><b>{i.title}</b></a><div class="mono faint break">{a.release.title}</div>{#if a.error}<div class="err">{a.error}</div>{/if}</td>
              <td class="muted" style="white-space:nowrap">{ago(a.updated_at)}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
{:else if tab === 'history'}
  {#if !history.length && loaded}<div class="card empty">No history yet.</div>{:else}
    <div class="card scroll-x">
      <table class="table">
        <tbody>
          {#each history as h (h.id)}
            <tr>
              <td style="white-space:nowrap"><span class="chip {tone(h.kind)}">{HISTORY[h.kind] ?? h.kind}</span></td>
              <td>
                {#if h.title_id}<a href="#/title/{h.title_id}"><b>{h.title ?? 'Removed title'}</b></a>{/if}
                {#if h.data.covers}<span class="muted"> {h.data.covers}</span>{/if}
                <div class="mono faint break">{h.data.release ?? h.data.path ?? ''}</div>
                {#if h.data.reason}<div class="muted small">{h.data.reason}</div>{/if}
              </td>
              <td class="muted" style="white-space:nowrap">{ago(h.ts)}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
{:else}
  {#if !blocklist.length && loaded}<div class="card empty">No releases are blocklisted.</div>{:else}
    <div class="card scroll-x">
      <table class="table">
        <tbody>
          {#each blocklist as b (b.id)}
            <tr>
              <td><a href="#/title/{b.title_id}"><b>{b.title ?? 'Removed title'}</b></a><div class="mono faint break">{b.release_title}</div><div class="muted small">{b.reason}</div></td>
              <td class="muted" style="white-space:nowrap">{ago(b.ts)}</td>
              <td><button class="btn small" onclick={async () => { await act(() => api.del(`/blocklist/${b.id}`), 'Allowed again'); load(); }}>Allow again</button></td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
{/if}

<style>
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 14px;
    flex-wrap: wrap;
    margin-bottom: 16px;
  }
  .speed {
    font-variant-numeric: tabular-nums;
  }
  .tabs {
    display: flex;
    gap: 22px;
    border-bottom: 1px solid var(--line);
    margin-bottom: 20px;
  }
  .tabs a {
    padding: 9px 0;
    color: var(--muted);
    font-weight: 550;
    border-bottom: 2px solid transparent;
    margin-bottom: -1px;
  }
  .tabs a.on {
    color: var(--text);
    border-color: var(--accent);
  }
  .notice {
    padding: 10px 14px;
    border-radius: 8px;
    background: color-mix(in srgb, var(--warn) 15%, transparent);
    margin-bottom: 14px;
    font-size: 14px;
  }
  .summary {
    padding: 14px 16px;
    margin-bottom: 14px;
    display: grid;
    gap: 10px;
  }
  .figures {
    gap: 8px 22px;
    font-variant-numeric: tabular-nums;
  }
  .figures b {
    font-size: 17px;
  }
  .low {
    color: var(--warn);
  }
  .item {
    display: flex;
    gap: 16px;
    padding: 15px;
    align-items: flex-start;
  }
  .item.blocked {
    border-color: color-mix(in srgb, var(--warn) 45%, var(--line));
  }
  .top {
    margin-bottom: 3px;
  }
  .t {
    font-weight: 650;
    font-size: 15.5px;
  }
  .rel {
    margin-bottom: 8px;
  }
  .why {
    font-size: 12.5px;
    margin-top: 7px;
  }
  .buttons {
    display: flex;
    flex-direction: column;
    gap: 6px;
    align-items: stretch;
  }
  .err {
    color: var(--bad);
    font-size: 13px;
    margin-top: 3px;
  }
  .small {
    font-size: 13px;
  }
  @media (max-width: 760px) {
    .item {
      flex-direction: column;
    }
    .buttons {
      flex-direction: row;
      flex-wrap: wrap;
    }
  }
</style>
