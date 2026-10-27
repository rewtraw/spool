<script lang="ts">
  import { api } from '../lib/api';
  import { app, act, go, loadTitles, signOut } from '../lib/state.svelte';
  import { ago, bytes, day, duration, pct, qualityName } from '../lib/format';
  import Poster from '../components/Poster.svelte';
  import JobLine from '../components/JobLine.svelte';

  let attention = $state<any[]>([]);
  // What the people who play have asked for and nobody has answered yet.
  let asks = $state<any[]>([]);
  let arrivals = $state<any[]>([]);
  let upcoming = $state<any[]>([]);
  let activity = $state<any[]>([]);
  let moves = $state<any>({ now: null, waiting: [] });
  let compact = $state<any>({ now: [], waiting: [], waiting_count: 0 });
  // Several machines may each be making one.
  const making = $derived<any[]>(compact.now ?? []);
  let loaded = $state(false);
  // What whoever is signed in was part-way through, and the next episode of what they watch.
  let watching = $state<any[]>([]);
  const apple = /Mac|iPhone|iPad|iPod/.test(navigator.platform || navigator.userAgent);
  const playLink = (w: any) => !apple || !w.tmdb_id ? '' : w.episode ? `infuse://series/${w.tmdb_id}-${w.episode.season}-${w.episode.episode}?play` : `infuse://movie/${w.tmdb_id}?play`;
  // Someone who only watches is shown what arrived lately from the library itself.
  const lately = $derived(app.manages ? [] : app.titles.filter((t) => (t.kind === 'movie' ? t.file_count > 0 : t.episodes_have > 0)).sort((a, b) => b.added_at - a.added_at).slice(0, 12));

  // Room on each place the library lives, not only where downloads land.
  let places = $state<any[]>([]);
  async function load() {
    if (app.me) api.get<any[]>('/watching').then((w) => (watching = w)).catch(() => {});
    if (!app.manages) {
      loaded = true;
      return;
    }
    const [a, h, c, act_] = await Promise.all([
      api.get('/attention').catch(() => []),
      api.get('/history?limit=300').catch(() => []),
      api.get('/calendar').catch(() => []),
      api.get('/activity').catch(() => ({ items: [] })),
    ]);
    api.get<any[]>('/locations').then((p) => (places = p)).catch(() => {});
    api.get<any[]>('/asks').then((a) => (asks = a.filter((x) => x.status === 'asked'))).catch(() => {});
    attention = a;
    arrivals = h.filter((x: any) => x.kind === 'imported' || x.kind === 'upgraded').slice(0, 12);
    const today = new Date().toISOString().slice(0, 10);
    upcoming = c.filter((x: any) => x.date >= today && x.monitored && !x.have).slice(0, 14);
    activity = act_.items.filter((i: any) => ['downloading', 'importing'].includes(i.acquisition.state));
    moves = act_.moves ?? moves;
    compact = act_.compact ?? compact;
    loaded = true;
  }
  // A move sends no events as it goes, so its line is asked for again while one is under way.
  $effect(() => {
    if (!moves.now && !moves.waiting.length && !making.length && !compact.waiting_count) return;
    const timer = setInterval(async () => {
      const a = await api.get('/activity').catch(() => null);
      if (a) {
        moves = a.moves ?? { now: null, waiting: [] };
        compact = a.compact ?? { now: [], waiting: [], waiting_count: 0 };
      }
    }, 5000);
    return () => clearInterval(timer);
  });

  $effect(() => {
    app.tick;
    load();
  });

  const titleOf = (id: number) => app.titles.find((t) => t.id === id);

  async function resolve(item: any, action: string) {
    const acq = item.item.acquisition_id;
    if (action === 'dismiss' || !acq) await act(() => api.post(`/attention/${item.item.id}/dismiss`));
    else if (action === 'retry') await act(() => api.post(`/activity/${acq}/import_anyway`), 'Importing');
    else if (action === 'series') {
      let made: any;
      const ok = await act(async () => { made = await api.post(`/titles/${item.item.title_id}/to-series`); }, 'Now a series. Looking for its episodes.');
      if (ok && made) await loadTitles();
    } else if (action === 'discard') await act(() => api.post(`/activity/${acq}/cancel_blocklist`), 'Discarded. Looking for another release.');
    load();
  }

  async function answer(a: any, how: 'add' | 'decline') {
    await act(() => api.post(`/asks/${a.id}/${how}`), how === 'add' ? `Added ${a.title}. Looking for it.` : `${a.account} is told: not this time`);
    if (how === 'add') await loadTitles();
    load();
  }

  async function dismissAll() {
    if (!confirm(`Dismiss all ${attention.length} notices? Nothing is deleted; anything still wrong will be raised again.`)) return;
    await act(() => api.post('/attention/dismiss-all'), 'Dismissed');
    load();
  }

  const stats = $derived(app.status ? [
    { label: 'Movies', value: app.status.movies },
    { label: 'Series', value: app.status.series },
    { label: 'In library', value: bytes(app.status.library_bytes) },
    ...(places.length > 1
      ? places.map((p) => ({ label: `Free on ${p.name}`, value: !p.online ? 'Away' : p.free != null ? bytes(p.free) : '—' }))
      : [{ label: 'Free', value: app.status.free_bytes != null ? bytes(app.status.free_bytes) : '—' }]),
  ] : []);
</script>

<header class="head">
  <h1>Home</h1>
  {#if app.manages}
    <div class="stats">
      {#each stats as s}<div><b>{s.value}</b><span>{s.label}</span></div>{/each}
    </div>
  {/if}
  {#if app.me && app.signInRequired}<div class="me faint small">{app.me.name} · <button class="link" onclick={signOut}>Sign out</button></div>{/if}
</header>

{#if app.manages && app.status?.mode === 'shadow'}
  <div class="notice">
    <b>Shadow mode.</b> Spool is reading the indexer feeds and recording what it would do. It does not download, move or delete anything.
    <a href="#/settings">Change in Settings</a>
  </div>
{/if}
{#each app.manages ? (app.status?.warnings ?? []) : [] as w}
  <div class="notice warn">{w}</div>
{/each}

{#if watching.length}
  <section>
    <div class="section-title">Continue watching</div>
    <div class="shelf">
      {#each watching as w (`${w.title_id}-${w.episode?.id ?? 0}`)}
        {@const t = app.titles.find((x) => x.id === w.title_id)}
        <div class="tile">
          <a href="#/title/{w.title_id}" class="art">
            <Poster src={t?.poster_small ?? w.poster} title={w.title} kind={w.kind} />
            {#if w.position && w.runtime}<span class="part"><i style="width:{Math.max(4, pct(w.position, w.runtime))}%"></i></span>{/if}
          </a>
          <a class="truncate name" href="#/title/{w.title_id}">{w.title}</a>
          <div class="faint small truncate">
            {#if w.episode}{w.next ? 'Next: ' : ''}S{w.episode.season} E{w.episode.episode}{w.episode.title ? ` · ${w.episode.title}` : ''}{:else if w.position && w.runtime}{duration(Math.max(0, w.runtime - w.position))} left{/if}
          </div>
          {#if playLink(w)}<a class="btn small play" href={playLink(w)}>{w.position ? 'Resume' : 'Play'}</a>{/if}
        </div>
      {/each}
    </div>
  </section>
{/if}

{#if !app.manages}
  <section>
    <div class="section-title">Lately in the library</div>
    {#if lately.length}
      <div class="shelf">
        {#each lately as t (t.id)}
          <a class="tile" href="#/title/{t.id}">
            <Poster src={t.poster_small ?? t.poster} title={t.title} kind={t.kind} />
            <div class="truncate name">{t.title}</div>
            <div class="faint small">{t.year || ''}{t.kind === 'series' ? ' · Series' : ''}</div>
          </a>
        {/each}
      </div>
    {:else}
      <div class="card empty">Nothing in the library yet.</div>
    {/if}
  </section>
{:else}




{#if asks.length}
  <section>
    <div class="row"><div class="section-title grow">Asked for</div><a class="more" href="#/settings/players">Everything asked for</a></div>
    <div class="stack">
      {#each asks as a (a.id)}
        <div class="card attention asked">
          <div class="thumb"><Poster src={a.poster} title={a.title} kind={a.kind} /></div>
          <div class="grow">
            <div class="row wrap"><span class="t">{a.title}</span><span class="faint">{[a.year || '', a.kind === 'movie' ? 'Film' : 'Series'].filter(Boolean).join(' · ')}</span></div>
            <p class="msg"><b>{a.account}</b> asked for this {ago(a.asked_at)}.</p>
            <div class="row wrap actions">
              <button class="btn small primary" onclick={() => answer(a, 'add')}>Add it</button>
              <button class="btn small ghost" onclick={() => answer(a, 'decline')}>Not this time</button>
            </div>
          </div>
        </div>
      {/each}
    </div>
  </section>
{/if}

{#if attention.length}
  <section>
    <div class="row"><div class="section-title grow">Needs attention</div>{#if attention.length > 1}<button class="btn small ghost" onclick={dismissAll}>Dismiss all {attention.length}</button>{/if}</div>
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
                {#if titleOf(a.item.title_id)?.as_series}<button class="btn small primary" onclick={() => resolve(a, 'series')}>Switch to series</button>{/if}
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

{#if activity.length || moves.now || moves.waiting.length || making.length || compact.waiting_count}
  <section>
    <div class="row"><div class="section-title grow">In progress</div><a class="more" href="#/activity">All activity</a></div>
    <div class="card list">
      {#if moves.now || moves.waiting.length}
        {@const m = moves.now}
        <a class="line" href="#/activity">
          <div class="grow">
            <div class="truncate">
              {#if m}<b>{m.title}</b> <span class="muted">being moved{m.to ? ` to ${m.to}` : ''}</span>{:else}<b>Titles waiting to be moved</b>{/if}
              {#if moves.waiting.length}<span class="muted"> · {moves.waiting.length} more waiting</span>{/if}
            </div>
            {#if m?.total}
              <div class="progress" style="margin:8px 0 5px" role="progressbar" aria-label="Move progress" aria-valuenow={Math.round(pct(m.done, m.total))} aria-valuemin="0" aria-valuemax="100"><i style="width:{pct(m.done, m.total)}%"></i></div>
              <div class="faint small">{bytes(m.done)} of {bytes(m.total)}</div>
            {/if}
          </div>
        </a>
      {/if}
      {#if making.length || compact.waiting_count}
        {@const c = making[0]}
        <a class="line" href="#/activity">
          <div class="grow">
            <div class="truncate">
              {#if c}<b>{c.title}{c.episode ? ` ${c.episode}` : ''}</b> <span class="muted">compact version being made</span>{#if making.length > 1}<span class="muted"> · {making.length - 1} more being made</span>{/if}{:else}<b>Compact versions waiting to be made</b>{/if}
              {#if compact.waiting_count}<span class="muted"> · {compact.waiting_count} more waiting</span>{/if}
            </div>
            {#if c?.total_ms}
              <div class="progress" style="margin:8px 0 5px" role="progressbar" aria-label="Compact version progress" aria-valuenow={Math.round(pct(c.done_ms, c.total_ms))} aria-valuemin="0" aria-valuemax="100"><i style="width:{pct(c.done_ms, c.total_ms)}%"></i></div>
              <div class="faint small">{Math.round(pct(c.done_ms, c.total_ms))}%{c.speed > 0 ? ` · ${c.speed.toFixed(1)}× the speed it plays` : ''}</div>
            {/if}
          </div>
        </a>
      {/if}
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
          <Poster src={t?.poster_small ?? t?.poster} title={h.title ?? ''} kind={t?.kind} />
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
  .asked {
    border-color: color-mix(in srgb, var(--accent) 45%, var(--line));
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
  @media (hover: hover) {
    .line:hover {
      background: var(--raised);
    }
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
  .tile .art {
    position: relative;
    display: block;
  }
  .part {
    position: absolute;
    left: 6px;
    right: 6px;
    bottom: 5px;
    height: 4px;
    border-radius: 2px;
    background: rgba(0, 0, 0, 0.5);
    overflow: hidden;
  }
  .part i {
    display: block;
    height: 100%;
    background: var(--accent);
  }
  .play {
    justify-self: start;
  }
  /* On a phone the menu has no room for who is signed in, so Home says. */
  .me {
    display: none;
    flex-basis: 100%;
    margin-top: -12px;
  }
  .me .link {
    color: inherit;
    text-decoration: underline;
  }
  .small {
    font-size: 12px;
  }
  @media (max-width: 760px) {
    .me {
      display: block;
    }
    .stats {
      gap: 18px;
    }
    .date {
      width: 86px;
    }
  }
</style>
