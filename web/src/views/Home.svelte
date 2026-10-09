<script lang="ts">
  import { api } from '../lib/api';
  import { app, act, go } from '../lib/state.svelte';
  import { ago, bytes, day, qualityName } from '../lib/format';
  import Poster from '../components/Poster.svelte';
  import JobLine from '../components/JobLine.svelte';

  let attention = $state<any[]>([]);
  let arrivals = $state<any[]>([]);
  let upcoming = $state<any[]>([]);
  let activity = $state<any[]>([]);
  let loaded = $state(false);

  async function load() {
    const [a, h, c, act_] = await Promise.all([
      api.get('/attention').catch(() => []),
      api.get('/history?limit=300').catch(() => []),
      api.get('/calendar').catch(() => []),
      api.get('/activity').catch(() => ({ items: [] })),
    ]);
    attention = a;
    arrivals = h.filter((x: any) => x.kind === 'imported' || x.kind === 'upgraded').slice(0, 12);
    const today = new Date().toISOString().slice(0, 10);
    upcoming = c.filter((x: any) => x.date >= today && x.monitored && !x.have).slice(0, 14);
    activity = act_.items.filter((i: any) => ['downloading', 'importing'].includes(i.acquisition.state));
    loaded = true;
  }

  $effect(() => {
    app.tick;
    load();
  });

  const titleOf = (id: number) => app.titles.find((t) => t.id === id);

  async function resolve(item: any, action: string) {
    const acq = item.item.acquisition_id;
    if (action === 'dismiss' || !acq) await act(() => api.post(`/attention/${item.item.id}/dismiss`));
    else if (action === 'retry') await act(() => api.post(`/activity/${acq}/retry_import`), 'Trying the import again');
    else if (action === 'discard') await act(() => api.post(`/activity/${acq}/cancel_blocklist`), 'Discarded. Looking for another release.');
    load();
  }

  const stats = $derived(app.status ? [
    { label: 'Movies', value: app.status.movies },
    { label: 'Series', value: app.status.series },
    { label: 'In library', value: bytes(app.status.library_bytes) },
    { label: 'Free', value: app.status.free_bytes != null ? bytes(app.status.free_bytes) : '—' },
  ] : []);
</script>

<header class="head">
  <h1>Home</h1>
  <div class="stats">
    {#each stats as s}<div><b>{s.value}</b><span>{s.label}</span></div>{/each}
  </div>
</header>

{#if app.status?.mode === 'shadow'}
  <div class="notice">
    <b>Shadow mode.</b> Spool is reading the indexer feeds and recording what it would do. It does not download, move or delete anything.
    <a href="#/settings">Change in Settings</a>
  </div>
{/if}
{#each app.status?.warnings ?? [] as w}
  <div class="notice warn">{w}</div>
{/each}

{#if attention.length}
  <section>
    <div class="section-title">Needs attention</div>
    <div class="stack">
      {#each attention as a (a.item.id)}
        <div class="card attention">
          <div class="thumb"><Poster src={a.poster} title={a.title ?? 'Spool'} kind={a.kind} /></div>
          <div class="grow">
            <div class="row wrap">
              {#if a.title}<a class="t" href="#/title/{a.item.title_id}">{a.title}</a>{/if}
              <span class="faint">{ago(a.item.ts)}</span>
            </div>
            <p class="msg">{a.item.message}</p>
            {#if a.item.data?.release}<p class="mono faint break">{a.item.data.release}</p>{/if}
            {#if a.item.data?.reason}<p class="faint break">{a.item.data.reason}</p>{/if}
            <div class="row wrap actions">
              {#if a.item.kind === 'import_blocked' && a.item.acquisition_id}
                <button class="btn small" onclick={() => resolve(a, 'retry')}>Import anyway</button>
                <button class="btn small" onclick={() => resolve(a, 'discard')}>Discard and find another</button>
              {/if}
              {#if a.item.kind === 'space'}<a class="btn small" href="#/settings/space">See what can be removed</a>{/if}
              <button class="btn small ghost" onclick={() => resolve(a, 'dismiss')}>Dismiss</button>
            </div>
          </div>
        </div>
      {/each}
    </div>
  </section>
{/if}

{#if activity.length}
  <section>
    <div class="row"><div class="section-title grow">In progress</div><a class="more" href="#/activity">All activity</a></div>
    <div class="card list">
      {#each activity as i (i.acquisition.id)}
        <a class="line" href="#/title/{i.acquisition.title_id}">
          <div class="grow">
            <div class="truncate"><b>{i.title}</b> <span class="muted">{qualityName(i.acquisition.quality, i.kind)}</span></div>
            <JobLine job={app.jobs[i.acquisition.job_id] ?? i.job} state={i.acquisition.state} error={i.acquisition.error} size={i.acquisition.release.size} />
          </div>
        </a>
      {/each}
    </div>
  </section>
{/if}

<section>
  <div class="section-title">Arrived</div>
  {#if arrivals.length}
    <div class="shelf">
      {#each arrivals as h (h.id)}
        {@const t = titleOf(h.title_id)}
        <a class="tile" href="#/title/{h.title_id}">
          <Poster src={t?.poster} title={h.title ?? ''} kind={t?.kind} />
          <div class="truncate name">{h.title}</div>
          <div class="faint small">{ago(h.ts)}{h.kind === 'upgraded' ? ' · upgraded' : ''}</div>
        </a>
      {/each}
    </div>
  {:else if loaded}
    <div class="card empty">Nothing has arrived through Spool yet.</div>
  {/if}
</section>

<section>
  <div class="section-title">Coming up</div>
  {#if upcoming.length}
    <div class="card list">
      {#each upcoming as u}
        <a class="line" href="#/title/{u.title_id}">
          <div class="date">{day(u.date)}</div>
          <div class="grow truncate"><b>{u.title}</b> <span class="muted">{u.label}</span></div>
        </a>
      {/each}
    </div>
  {:else if loaded}
    <div class="card empty">Nothing monitored is due in the next four weeks.</div>
  {/if}
</section>

{#if loaded && !app.titles.length}
  <section>
    <div class="card empty">
      <p>The library is empty.</p>
      <p style="margin-top:10px"><button class="btn primary" onclick={() => go('discover')}>Add a movie or series</button></p>
    </div>
  </section>
{/if}

<style>
  .head {
    display: flex;
    align-items: flex-end;
    justify-content: space-between;
    gap: 20px;
    flex-wrap: wrap;
    margin-bottom: 22px;
  }
  .stats {
    display: flex;
    gap: 26px;
  }
  .stats div {
    display: grid;
  }
  .stats b {
    font: 600 20px/1.2 var(--serif);
    font-variant-numeric: tabular-nums;
  }
  .stats span {
    font-size: 12px;
    color: var(--faint);
  }
  .notice {
    padding: 12px 15px;
    border-radius: 9px;
    background: var(--accent-soft);
    font-size: 14px;
    margin-bottom: 10px;
  }
  .notice.warn {
    background: color-mix(in srgb, var(--warn) 15%, transparent);
  }
  .notice a {
    color: var(--accent);
    font-weight: 550;
    white-space: nowrap;
  }
  section {
    margin-top: 30px;
    display: grid;
    gap: 12px;
  }
  .attention {
    display: flex;
    gap: 14px;
    padding: 14px;
    border-color: color-mix(in srgb, var(--warn) 45%, var(--line));
  }
  .thumb {
    width: 54px;
    flex: none;
  }
  .t {
    font-weight: 650;
  }
  .msg {
    margin: 4px 0;
  }
  .actions {
    margin-top: 10px;
  }
  .list {
    overflow: hidden;
  }
  .line {
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 12px 15px;
    border-bottom: 1px solid var(--line);
  }
  .line:last-child {
    border-bottom: none;
  }
  .line:hover {
    background: var(--raised);
  }
  .date {
    width: 104px;
    flex: none;
    color: var(--muted);
    font-size: 13.5px;
  }
  .more {
    color: var(--accent);
    font-size: 13.5px;
    font-weight: 550;
  }
  .shelf {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(118px, 1fr));
    gap: 16px 14px;
  }
  .tile {
    display: grid;
    gap: 5px;
    min-width: 0;
  }
  .name {
    font-weight: 550;
    font-size: 13.5px;
    margin-top: 2px;
  }
  .small {
    font-size: 12px;
  }
  @media (max-width: 760px) {
    .stats {
      gap: 18px;
    }
    .date {
      width: 86px;
    }
  }
</style>
