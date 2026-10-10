<script lang="ts">
  import { api, type Decision, type Episode, type MediaFile, type Title } from '../lib/api';
  import { app, act, go, loadTitles } from '../lib/state.svelte';
  import { age, ago, bytes, fullDate, pct, qualityName, STATE_LABEL } from '../lib/format';
  import Subtitles from '../components/Subtitles.svelte';
  import Poster from '../components/Poster.svelte';
  import JobLine from '../components/JobLine.svelte';

  let { id }: { id: number } = $props();

  type Detail = Title & { episodes: Episode[]; files: MediaFile[]; decisions: Decision[]; history: any[]; acquisitions: any[]; saved: any[]; plex?: { rating_key: string; url: string | null; section: string } };
  let t = $state<Detail | null>(null);
  let missing = $state(false);
  let searching = $state('');
  let searchMessage = $state('');
  let showRejected = $state(false);
  let openSeasons = $state<Record<number, boolean>>({});
  let confirmDelete = $state(false);
  let deleteFiles = $state(false);
  let recycleDays = $state(7);
  api.get('/recycle').then((r) => (recycleDays = r.days)).catch(() => {});

  async function load() {
    try {
      const d = await api.get<Detail>(`/titles/${id}`);
      const first = !t;
      t = d;
      if (first && d.kind === 'series') {
        const seasons = [...new Set(d.episodes.map((e) => e.season))].filter((s) => s > 0);
        const latest = Math.max(...seasons, 1);
        openSeasons = { [latest]: true };
      }
    } catch (e: any) {
      if (e.status === 404) missing = true;
    }
  }

  $effect(() => {
    app.titleTick[id];
    load();
  });

  const active = $derived(t?.acquisitions.filter((a) => ['downloading', 'importing', 'import_blocked'].includes(a.state)) ?? []);
  // What is on its way for each episode, in a word or two.
  const coming = $derived.by(() => {
    const map = new Map<number, string>();
    for (const a of active) {
      const job = a.job_id ? app.jobs[a.job_id] : null;
      const label =
        a.state === 'importing' ? 'Importing'
        : a.state === 'import_blocked' ? 'Needs attention'
        : !job || job.state === 'queued' ? 'Queued'
        : job.state === 'downloading' ? `Downloading ${Math.round(pct(job.done_bytes, job.total_bytes))}%`
        : job.state === 'paused' ? (job.message === 'Waiting for free space' ? 'Waiting for space' : 'Paused')
        : ['verifying', 'repairing', 'extracting', 'finishing'].includes(job.state) ? 'Unpacking'
        : 'Queued';
      for (const id of a.episode_ids ?? []) map.set(id, label);
    }
    return map;
  });
  const seasons = $derived.by(() => {
    if (!t) return [];
    const map = new Map<number, Episode[]>();
    for (const e of t.episodes) (map.get(e.season) ?? map.set(e.season, []).get(e.season)!).push(e);
    return [...map.entries()].sort((a, b) => (b[0] || -1) - (a[0] || -1));
  });
  const fileById = $derived(new Map((t?.files ?? []).map((f) => [f.id, f])));
  const accepted = $derived(t?.decisions.filter((d) => d.accepted) ?? []);
  const rejected = $derived(t?.decisions.filter((d) => !d.accepted) ?? []);
  const isActive = $derived(app.status?.mode === 'active');
  const ceiling = $derived((app.profiles.find((p) => p.id === t?.profile_id)?.target_size_gb?.[1] ?? 0) * 1024 ** 3);
  // Files above the profile's size target, and by how much: what a smaller copy could save.
  const oversize = $derived.by(() => {
    const over = ceiling ? (t?.files ?? []).filter((f) => f.size > ceiling) : [];
    return { count: over.length, bytes: over.reduce((n, f) => n + f.size - ceiling, 0) };
  });
  const today = new Date().toISOString().slice(0, 10);
  const aired = (e: Episode) => !!e.air_date && e.air_date <= today;

  const SAVED_FROM: Record<string, string> = { grabbed: 'Downloaded before', runner_up: 'Kept as a fallback', manual: 'Added by hand' };
  async function grabSaved(n: any) {
    await act(() => api.post(`/archive/${n.id}/grab`), 'Download started');
    load();
  }
  async function checkSaved(n: any) {
    const r = await act(() => api.post<any>(`/archive/${n.id}/check`));
    if (r) {
      n.available = r.available;
      n.checked_at = r.checked_at;
    }
  }
  async function removeSaved(n: any) {
    await act(() => api.del(`/archive/${n.id}`));
    load();
  }

  async function patch(body: any) {
    await act(() => api.patch(`/titles/${id}`, body));
    load();
    loadTitles();
  }

  async function search(scope: { season?: number; episode?: number; compact?: boolean } = {}, grab = true, key = 'all') {
    searching = key;
    searchMessage = '';
    const r = await act(() => api.post(`/titles/${id}/search`, { ...scope, grab: grab && isActive }));
    searching = '';
    if (r) {
      searchMessage = r.message;
      await load();
      document.getElementById('releases')?.scrollIntoView({ behavior: 'smooth', block: 'start' });
    }
  }

  let openSubs = $state<Record<number, boolean>>({});
  async function setExtra(f: any, extra: boolean) {
    await act(() => api.patch(`/files/${f.id}`, { extra }));
  }
  async function grab(d: Decision, anotherVersion = false) {
    await act(() => api.post(`/decisions/${d.id}/grab`, { another_version: anotherVersion }), anotherVersion ? 'Sent to download. It will be kept beside the file you have.' : 'Sent to download');
    load();
  }

  async function remove() {
    const ok = await act(() => api.del(`/titles/${id}?delete_files=${deleteFiles}`), 'Removed from the library');
    if (ok) {
      await loadTitles();
      go('library');
    }
  }

  async function toggleSeason(n: number, monitored: boolean) {
    await act(() => api.patch(`/titles/${id}/seasons/${n}`, { monitored }));
    load();
  }
  async function toggleEpisode(e: Episode) {
    await act(() => api.patch(`/episodes/${e.id}`, { monitored: !e.monitored }));
    load();
  }
  async function deleteFile(f: MediaFile) {
    const keep = recycleDays > 0;
    if (!confirm(`${keep ? `Delete this file? It is kept for ${recycleDays} days before it is removed for good.` : 'Permanently delete this file?'}\n\n${f.rel_path}`)) return;
    await act(() => api.del(`/files/${f.id}`), keep ? 'Deleted. Kept in the recycle folder for now.' : 'Deleted');
    load();
  }

  let fileInput = $state<HTMLInputElement>();
  async function upload() {
    const f = fileInput?.files?.[0];
    if (!f) return;
    await act(() => api.upload(`/titles/${id}/nzb?name=${encodeURIComponent(f.name)}`, f), 'Added to the queue');
    fileInput!.value = '';
    load();
  }

  const seasonOf = (n: number) => t?.seasons.find((s) => s.number === n)?.monitored ?? true;
  const media = (f: MediaFile) => [f.media_info?.dynamic_range_type, f.media_info?.video_codec, f.media_info ? `${f.media_info.audio_codec} ${f.media_info.audio_channels || ''}`.trim() : '', f.release_group].filter(Boolean).join(' · ');
  const HISTORY: Record<string, string> = { grabbed: 'Sent to download', would_grab: 'Would have grabbed', imported: 'Imported', upgraded: 'Upgraded', download_failed: 'Download failed', file_deleted: 'File deleted', file_missing: 'File went missing' };
</script>

{#if missing}
  <div class="card empty">This title is not in the library. <a href="#/library" style="color:var(--accent)">Back to the library</a></div>
{:else if !t}
  <div class="empty">Loading…</div>
{:else}
  <div class="hero" style={t.fanart ? `--art:url("${t.fanart}")` : ''}>
    <div class="art"><Poster src={t.poster} title={t.title} kind={t.kind} /></div>
    <div class="info">
      <a class="back" href="#/library">Library</a>
      <h1>{t.title}</h1>
      <div class="meta muted">
        {[t.year || '', t.kind === 'movie' ? (t.runtime ? `${t.runtime} min` : '') : t.network, t.kind === 'series' ? (t.status === 'ended' ? 'Ended' : t.status === 'continuing' ? 'Continuing' : '') : t.studio, t.genres.slice(0, 3).join(', '), t.original_language && t.original_language !== 'english' ? `In ${t.original_language[0].toUpperCase()}${t.original_language.slice(1)}` : ''].filter(Boolean).join(' · ')}
      </div>
      {#if t.overview}<p class="overview">{t.overview}</p>{/if}
      {#if (t.kind === 'movie' && (t.tmdb_id || t.imdb_id)) || t.plex?.url}
        <div class="links">
          {#if t.plex?.url}<a href={t.plex.url} target="_blank" rel="noopener noreferrer">Open in Plex <span aria-hidden="true">↗</span></a>{/if}
          {#if t.kind === 'movie' && (t.tmdb_id || t.imdb_id)}<a href={t.tmdb_id ? `https://letterboxd.com/tmdb/${t.tmdb_id}` : `https://letterboxd.com/imdb/${t.imdb_id}`} target="_blank" rel="noopener noreferrer">Letterboxd <span aria-hidden="true">↗</span></a>{/if}
        </div>
      {/if}
      <div class="controls">
        <label class="check"><input type="checkbox" checked={t.monitored} onchange={(e) => patch({ monitored: e.currentTarget.checked })} /> Monitored</label>
        <select class="input auto" value={t.profile_id} onchange={(e) => patch({ profile_id: Number(e.currentTarget.value) })} aria-label="Quality profile">
          {#each app.profiles.filter((p) => p.kind === (t!.kind === 'movie' ? 'movie' : 'tv')) as p}<option value={p.id}>{p.name}</option>{/each}
          {#if !app.profiles.some((p) => p.id === t!.profile_id)}<option value={t.profile_id}>No profile</option>{/if}
        </select>
        {#if t.kind === 'movie'}
          <select class="input auto" value={t.minimum_availability} onchange={(e) => patch({ minimum_availability: e.currentTarget.value })} aria-label="Get it once it is">
            <option value="announced">Get as soon as announced</option>
            <option value="in_cinemas">Get once in cinemas</option>
            <option value="released">Get once released for home</option>
          </select>
        {/if}
      </div>
      <div class="row wrap">
        <button class="btn primary" disabled={!!searching} onclick={() => search({}, true, 'all')}>
          {searching === 'all' ? 'Searching…' : !isActive ? 'Search' : t.kind === 'movie' ? 'Search and download' : 'Get missing episodes'}
        </button>
        {#if isActive}<button class="btn" disabled={!!searching} onclick={() => search({}, false, 'look')}>{searching === 'look' ? 'Searching…' : 'Show releases'}</button>{/if}
        {#if isActive && oversize.count}<button class="btn" disabled={!!searching} title="Replace {oversize.count === 1 ? 'the file' : `${oversize.count} files`} over this profile's size target with a smaller copy, even at lower quality" onclick={() => search({ compact: true }, true, 'compact')}>{searching === 'compact' ? 'Searching…' : `Find a smaller copy (${bytes(oversize.bytes)} over target)`}</button>{/if}
        <button class="btn ghost" onclick={async () => { await act(() => api.post(`/titles/${id}/refresh`), 'Details refreshed'); load(); }}>Refresh details</button>
        <button class="btn ghost" onclick={async () => { const r = await act(() => api.post(`/titles/${id}/scan`)); if (r) { searchMessage = `Scan: ${r.files_found} files on disk, ${r.files_added} added, ${r.files_missing} missing`; load(); } }}>Rescan folder</button>
        <button class="btn ghost danger" onclick={() => (confirmDelete = true)}>Remove</button>
      </div>
      {#if !isActive}<p class="faint small">Shadow mode: searching shows what Spool would choose, and downloads nothing.</p>{/if}
      {#if searchMessage}<p class="muted" role="status">{searchMessage}</p>{/if}
    </div>
  </div>

  {#if active.length}
    <section>
      <div class="section-title">In progress</div>
      <div class="stack">
        {#each active as a (a.id)}
          <div class="card pad">
            <div class="row wrap"><span class="chip {a.state === 'import_blocked' ? 'warn' : 'accent'}">{a.state === 'downloading' ? STATE_LABEL[app.jobs[a.job_id]?.state ?? 'queued'] : STATE_LABEL[a.state]}</span><span class="chip">{qualityName(a.quality, t.kind)}</span><span class="mono faint break grow">{a.release.title}</span></div>
            <div style="margin-top:9px"><JobLine job={app.jobs[a.job_id]} state={a.state} error={a.error} size={a.release.size} /></div>
            <div class="row wrap" style="margin-top:10px">
              {#if a.state === 'import_blocked'}<button class="btn small" onclick={async () => { await act(() => api.post(`/activity/${a.id}/retry_import`)); load(); }}>Import anyway</button>{/if}
              <button class="btn small" onclick={async () => { await act(() => api.post(`/activity/${a.id}/cancel_blocklist`), 'Removed. Looking for another release.'); load(); }}>Remove and find another</button>
              <button class="btn small ghost danger" onclick={async () => { await act(() => api.post(`/activity/${a.id}/cancel`), 'Removed'); load(); }}>Remove</button>
            </div>
          </div>
        {/each}
      </div>
    </section>
  {/if}

  {#if t.kind === 'movie'}
    <section>
      <div class="section-title">{t.files.length > 1 ? 'Versions' : 'File'}</div>
      {#if t.files.length}
        {#each t.files as f (f.id)}
          <div class="card pad file">
            <div class="grow">
              <div class="row wrap"><span class="chip ok">{qualityName(f.quality, 'movie')}</span>{#if f.edition}<span class="chip">{f.edition}</span>{/if}{#if t.files.length > 1}<span class="chip {f.extra ? '' : 'accent'}">{f.extra ? 'Extra version' : 'Main'}</span>{/if}<span class="muted">{bytes(f.size)}</span><span class="muted">{media(f)}</span></div>
              <div class="mono faint break" style="margin-top:6px">{t.path}/{f.rel_path}</div>
              <Subtitles fileId={f.id} />
            </div>
            {#if t.files.length > 1}<button class="btn small ghost" title={f.extra ? 'Upgrades and smaller copies will replace this file' : 'Keep this file whatever else is downloaded; upgrades replace only the main file'} onclick={() => setExtra(f, !f.extra)}>{f.extra ? 'Make main' : 'Keep as extra'}</button>{/if}
            <button class="btn small ghost danger" onclick={() => deleteFile(f)}>Delete</button>
          </div>
        {/each}
        {#if t.files.length > 1}<p class="faint small" style="margin-top:8px">Upgrades and smaller copies replace the main file. Extra versions stay until you delete them. To add one, use "Keep both" on a release below.</p>{/if}
      {:else}
        <div class="card empty">
          {#if !t.available}Not released yet{#if t.digital_release || t.physical_release || t.in_cinemas}. {t.digital_release ? `Digital release ${fullDate(t.digital_release)}` : t.physical_release ? `Physical release ${fullDate(t.physical_release)}` : `In cinemas ${fullDate(t.in_cinemas)}`}{/if}.
          {:else if t.monitored}No file yet. Spool is watching the indexer feeds for an acceptable release.
          {:else}No file, and not monitored.{/if}
        </div>
      {/if}
    </section>
  {:else}
    <section>
      <div class="section-title">Episodes</div>
      <div class="stack">
        {#each seasons as [n, eps] (n)}
          {@const have = eps.filter((e) => e.file_id).length}
          {@const airedCount = eps.filter(aired).length}
          <div class="card season">
            <div class="shead">
              <button class="toggle" onclick={() => (openSeasons[n] = !openSeasons[n])} aria-expanded={!!openSeasons[n]}>
                <span class="caret" class:open={openSeasons[n]}>›</span>
                <b>{n === 0 ? 'Specials' : `Season ${n}`}</b>
                <span class="chip {have >= airedCount && airedCount > 0 ? 'ok' : have < airedCount && seasonOf(n) ? 'warn' : ''}">{have} / {airedCount}{airedCount < eps.length ? ` of ${eps.length}` : ''}</span>
              </button>
              <label class="check small"><input type="checkbox" checked={seasonOf(n)} onchange={(e) => toggleSeason(n, e.currentTarget.checked)} /> Monitored</label>
              {#if n > 0}<button class="btn small" disabled={!!searching} onclick={() => search({ season: n }, true, `s${n}`)}>{searching === `s${n}` ? 'Searching…' : 'Search season'}</button>{/if}
            </div>
            {#if openSeasons[n]}
              <div class="eps">
                {#each eps as e (e.id)}
                  {@const f = e.file_id ? fileById.get(e.file_id) : null}
                  <div class="ep" class:dim={!e.monitored && !f}>
                    <input type="checkbox" checked={e.monitored} onchange={() => toggleEpisode(e)} aria-label="Monitor episode {e.episode}" />
                    <span class="num">{e.episode}</span>
                    <div class="grow">
                      <div class="truncate">{e.title || 'To be announced'}</div>
                      <div class="faint small">{e.air_date ? fullDate(e.air_date) : 'No air date'}{f ? ` · ${bytes(f.size)}${f.release_group ? ' · ' + f.release_group : ''}` : ''}</div>
                    </div>
                    {#if f}<span class="chip ok">{qualityName(f.quality, 'series')}</span>{/if}
                    {#if coming.get(e.id)}<span class="chip accent">{coming.get(e.id)}</span>
                    {:else if !f && !aired(e)}<span class="chip">Not aired</span>
                    {:else if !f && e.monitored}<span class="chip warn">Missing</span>{/if}
                    {#if aired(e)}<button class="btn small ghost" disabled={!!searching} onclick={() => search({ episode: e.id }, true, `e${e.id}`)} aria-label="Search for episode {e.episode}">{searching === `e${e.id}` ? '…' : 'Search'}</button>{/if}
                    {#if f}<button class="btn small ghost" aria-expanded={!!openSubs[e.id]} onclick={() => (openSubs[e.id] = !openSubs[e.id])}>Subtitles</button>{/if}
                    {#if f}<button class="btn small ghost danger" onclick={() => deleteFile(f)}>Delete</button>{/if}
                  </div>
                  {#if f && openSubs[e.id]}<div class="epsubs"><Subtitles fileId={f.id} /></div>{/if}
                {/each}
              </div>
            {/if}
          </div>
        {/each}
        {#if !seasons.length}<div class="card empty">No episodes are known for this series yet.</div>{/if}
      </div>
    </section>
  {/if}

  <section id="releases">
    <div class="row wrap">
      <div class="section-title grow">Releases considered</div>
      <label class="btn small ghost upload">Add an NZB<input type="file" accept=".nzb" bind:this={fileInput} onchange={upload} hidden /></label>
    </div>
    {#if !t.decisions.length}
      <div class="card empty">No releases have been looked at yet. They appear here as Spool sees them in the feeds, or when you search.</div>
    {:else}
      <div class="card scroll-x">
        <table class="table">
          <thead><tr><th>Release</th><th>Quality</th><th>Size</th><th>Age</th><th>Verdict</th><th></th></tr></thead>
          <tbody>
            {#each [...accepted, ...(showRejected ? rejected : [])] as d (d.id)}
              <tr class:rej={!d.accepted}>
                <td><div class="mono break rel">{d.release.title}</div><div class="faint small">{d.release.indexer}{d.covers ? ` · ${d.covers}` : ''}{d.languages.length ? ` · ${d.languages.join(', ')}` : ''}</div></td>
                <td style="white-space:nowrap">{qualityName(d.quality, t.kind)}</td>
                <td class="muted" style="white-space:nowrap">{bytes(d.release.size)}</td>
                <td class="muted" style="white-space:nowrap">{age(d.release.published)}</td>
                <td class="verdict">
                  {#if d.accepted}<span class="chip ok">{d.id === accepted[0]?.id ? 'Best choice' : 'Acceptable'}</span>{#if ceiling && d.release.size > ceiling}<div class="faint small">Over the profile's size target; used only if nothing smaller fits.</div>{/if}
                  {:else}{#each d.rejections as r}<div class="reason">{r.message}</div>{/each}{/if}
                </td>
                <td>{#if isActive}<div class="row" style="gap:6px;flex-wrap:nowrap"><button class="btn small" onclick={() => grab(d)}>{d.accepted ? 'Download' : 'Download anyway'}</button>{#if t.kind === 'movie' && t.files.length}<button class="btn small ghost" title="Download this and keep it beside the file you already have, as another version of the film" onclick={() => grab(d, true)}>Keep both</button>{/if}</div>{/if}</td>
              </tr>
            {/each}
          </tbody>
        </table>
        {#if !accepted.length && !showRejected}<div class="empty" style="padding:20px">None of the {rejected.length} releases seen is acceptable.</div>{/if}
      </div>
      {#if rejected.length}
        <button class="btn small ghost" onclick={() => (showRejected = !showRejected)}>{showRejected ? 'Hide' : 'Show'} {rejected.length} rejected, with reasons</button>
      {/if}
    {/if}
  </section>

  {#if t.saved?.length}
    <section id="saved">
      <div class="section-title">Saved releases</div>
      <div class="card scroll-x">
        <table class="table">
          <thead><tr><th>Release</th><th>Quality</th><th>Size</th><th>Saved</th><th>On Usenet</th><th></th></tr></thead>
          <tbody>
            {#each t.saved as n (n.id)}
              <tr>
                <td><div class="mono break rel">{n.release.title}</div><div class="faint small">{SAVED_FROM[n.source] ?? n.source}{n.covers ? ` · ${n.covers}` : ''}</div></td>
                <td style="white-space:nowrap">{qualityName(n.quality, t.kind)}</td>
                <td class="muted" style="white-space:nowrap">{bytes(n.release.size)}</td>
                <td class="muted" style="white-space:nowrap">{ago(n.fetched_at)}</td>
                <td style="white-space:nowrap">
                  {#if !n.available}<span class="faint">Not checked</span>
                  {:else if n.available[1] > 0 && n.available[0] * 100 >= n.available[1] * 90}<span class="chip ok">Available</span>
                  {:else}<span class="chip bad">Gone</span>{/if}
                  {#if n.checked_at}<div class="faint small">{ago(n.checked_at)}</div>{/if}
                </td>
                <td>
                  <div class="row saved-actions">
                    {#if isActive}<button class="btn small" onclick={() => grabSaved(n)}>Download</button>{/if}
                    <button class="btn small ghost" onclick={() => checkSaved(n)}>Check</button>
                    <button class="btn small ghost" onclick={() => removeSaved(n)}>Remove</button>
                  </div>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
      <p class="faint small">Spool keeps the NZB of every release it downloads, and of the next-best options, so they can be fetched again later without an indexer.</p>
    </section>
  {/if}

  {#if t.history.length}
    <section>
      <div class="section-title">History</div>
      <div class="card scroll-x">
        <table class="table">
          <tbody>
            {#each t.history.slice(0, 40) as h (h.id)}
              <tr>
                <td style="white-space:nowrap"><span class="chip {h.kind === 'download_failed' ? 'bad' : h.kind === 'imported' || h.kind === 'upgraded' ? 'ok' : h.kind === 'would_grab' ? 'warn' : ''}">{HISTORY[h.kind] ?? h.kind}</span></td>
                <td><div class="mono faint break">{h.data.release ?? h.data.path ?? ''}</div>{#if h.data.reason}<div class="muted small">{h.data.reason}</div>{/if}</td>
                <td class="muted" style="white-space:nowrap">{ago(h.ts)}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    </section>
  {/if}

  <p class="faint mono path break">{t.path}</p>

  {#if confirmDelete}
    <div class="scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && (confirmDelete = false)}>
      <div class="card dialog" role="dialog" aria-modal="true" aria-label="Remove {t.title}">
        <h2>Remove {t.title}?</h2>
        <p class="muted">Spool will forget this title and stop looking for it.</p>
        <label class="check"><input type="checkbox" bind:checked={deleteFiles} /> Also delete its {t.file_count} {t.file_count === 1 ? 'file' : 'files'}{recycleDays > 0 ? ` (kept ${recycleDays} days in the recycle folder, then removed)` : ' permanently'}</label>
        <div class="row" style="justify-content:flex-end">
          <button class="btn ghost" onclick={() => (confirmDelete = false)}>Cancel</button>
          <button class="btn danger" onclick={remove}>Remove</button>
        </div>
      </div>
    </div>
  {/if}
{/if}

<style>
  .saved-actions {
    gap: 6px;
    flex-wrap: nowrap;
  }
  .links {
    display: flex;
    gap: 16px;
    flex-wrap: wrap;
  }
  .hero {
    position: relative;
    display: grid;
    grid-template-columns: 190px minmax(0, 1fr);
    gap: 28px;
    padding: 26px;
    margin: -8px 0 6px;
    border-radius: 14px;
    overflow: hidden;
    border: 1px solid var(--line);
    background: var(--surface);
    isolation: isolate;
  }
  .hero::before {
    content: '';
    position: absolute;
    inset: 0;
    background-image: linear-gradient(90deg, var(--surface) 22%, color-mix(in srgb, var(--surface) 72%, transparent) 60%, color-mix(in srgb, var(--surface) 88%, transparent)), var(--art, none);
    background-size: cover;
    background-position: center 20%;
    z-index: -1;
  }
  .info {
    display: grid;
    gap: 12px;
    align-content: start;
    min-width: 0;
  }
  .back {
    color: var(--muted);
    font-size: 13px;
    font-weight: 550;
  }
  .back::before {
    content: '‹ ';
  }
  .meta {
    font-size: 14px;
  }
  .overview {
    max-width: 72ch;
    color: var(--muted);
    display: -webkit-box;
    -webkit-line-clamp: 4;
    line-clamp: 4;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .links a {
    color: var(--accent);
    font-size: 14px;
    font-weight: 550;
  }
  .links a:hover {
    text-decoration: underline;
  }
  .controls {
    display: flex;
    gap: 12px;
    flex-wrap: wrap;
    align-items: center;
  }
  .auto {
    width: auto;
  }
  section {
    margin-top: 30px;
    display: grid;
    gap: 12px;
  }
  .pad {
    padding: 14px 15px;
  }
  .file {
    display: flex;
    gap: 12px;
    align-items: flex-start;
  }
  .season {
    overflow: hidden;
  }
  .shead {
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 8px 12px 8px 6px;
    flex-wrap: wrap;
  }
  .toggle {
    display: flex;
    align-items: center;
    gap: 10px;
    flex: 1;
    min-width: 160px;
    height: 36px;
    padding: 0 8px;
    background: none;
    border: none;
    cursor: pointer;
    text-align: left;
    border-radius: 6px;
  }
  .caret {
    display: inline-block;
    width: 12px;
    color: var(--faint);
    font-size: 18px;
    transition: transform 0.12s;
  }
  .caret.open {
    transform: rotate(90deg);
  }
  .eps {
    border-top: 1px solid var(--line);
  }
  .ep {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 9px 14px;
    border-bottom: 1px solid var(--line);
  }
  .epsubs {
    padding: 0 14px 12px 52px;
    border-bottom: 1px solid var(--line);
  }
  .ep:last-child {
    border-bottom: none;
  }
  .ep.dim {
    opacity: 0.55;
  }
  .num {
    width: 24px;
    text-align: right;
    color: var(--faint);
    font-variant-numeric: tabular-nums;
  }
  .small {
    font-size: 12.5px;
  }
  .rel {
    min-width: 240px;
  }
  tr.rej td {
    color: var(--muted);
  }
  .verdict {
    min-width: 230px;
  }
  .reason {
    font-size: 13.5px;
    color: var(--text);
  }
  .reason + .reason {
    margin-top: 3px;
  }
  .upload {
    cursor: pointer;
  }
  .path {
    margin-top: 26px;
  }
  .scrim {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.55);
    display: grid;
    place-items: center;
    padding: 18px;
    z-index: 40;
  }
  .dialog {
    width: min(440px, 100%);
    padding: 22px;
    display: grid;
    gap: 14px;
    box-shadow: var(--shadow);
  }
  @media (max-width: 760px) {
    .hero {
      grid-template-columns: 96px minmax(0, 1fr);
      gap: 14px;
      padding: 14px;
    }
    .overview {
      -webkit-line-clamp: 3;
      line-clamp: 3;
    }
    .hero :global(.btn) {
      white-space: normal;
      height: auto;
      min-height: 36px;
      text-align: left;
    }
    .ep {
      flex-wrap: wrap;
    }
  }
</style>
