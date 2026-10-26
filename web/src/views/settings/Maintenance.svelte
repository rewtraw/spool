<script lang="ts">
  // The Maintenance tab: tasks, the recycle folder and where files are.
  import { api } from '../../lib/api';
  import { app, act, loadStatus, toast } from '../../lib/state.svelte';
  import { ago, bytes } from '../../lib/format';

  let paths = $state<any>(null);
  let recycle = $state<any>(null);
  async function loadRecycle() {
    recycle = await api.get('/recycle').catch(() => null);
  }
  async function emptyRecycle() {
    if (!confirm(`Permanently delete ${recycle.files} recycled ${recycle.files === 1 ? 'file' : 'files'} (${bytes(recycle.bytes)})?`)) return;
    const r = await act(() => api.post('/recycle/empty'));
    if (r) toast(`Freed ${bytes(r.freed)}`);
    loadRecycle();
    loadStatus();
  }
  // ---- maintenance
  const TASKS = [
    ['rss', 'Read indexer feeds', 'Checks every indexer for new releases the library wants.'],
    ['backlog', 'Search for missing titles', 'Searches a few missing titles, least recently searched first.'],
    ['refresh', 'Refresh details', 'Updates titles and episode lists from TMDB and TVmaze.'],
    ['scan', 'Rescan library folders', 'Finds files added or removed outside Spool. Changes the catalog only.'],
    ['housekeeping', 'Back up and tidy', 'Writes a database backup and empties old recycle folders.'],
    ['plex', 'Read Plex', 'Matches Plex\'s films and shows to titles in Spool.'],
    ['anime', 'Read AniList', 'Finds which titles are anime and reads their format, names and relations from AniList. Collections of series and films are built from these.'],
    ['subtitles', 'Find missing subtitles', 'Looks for subtitles in the languages you chose for a few files that lack them, and keeps only ones in time with the film.'],
  ];
  async function runTask(name: string) {
    await act(() => api.post(`/tasks/${name}`), 'Started');
  }
  async function checkPaths() {
    paths = 'running';
    paths = await api.get('/check-paths').catch((e) => ({ error: e.message }));
  }
  loadRecycle();
</script>

  <div class="stack form">
    <div class="card list">
      {#each TASKS as [name, label, hint]}
        {@const s = app.status?.tasks?.[name]}
        <div class="task">
          <div class="grow"><b>{label}</b><div class="faint small">{hint}</div>{#if s?.last_message}<div class="muted small">{s.running ? 'Running…' : `${ago(s.last_run)}: ${s.last_message}`}</div>{/if}</div>
          <button class="btn small" disabled={s?.running} onclick={() => runTask(name)}>{s?.running ? 'Running…' : 'Run now'}</button>
        </div>
      {/each}
    </div>
    <fieldset class="card">
      <legend>Recycled files</legend>
      {#if recycle}
        <p class="muted">
          {#if recycle.files}<b>{bytes(recycle.bytes)}</b> in {recycle.files} replaced or deleted {recycle.files === 1 ? 'file' : 'files'}.{:else}Nothing is waiting to be removed.{/if}
          {#if recycle.days > 0}Files are removed automatically after {recycle.days} {recycle.days === 1 ? 'day' : 'days'}.{:else}Recycling is off: files are deleted at once.{/if}
          <a class="link" href="#/settings/general">Change</a>
        </p>
        <div><button class="btn" disabled={!recycle.files} onclick={emptyRecycle}>Delete them now</button></div>
      {/if}
    </fieldset>
    <fieldset class="card">
      <legend>File locations</legend>
      <p class="muted">Compares where every library file is with where Spool's naming settings would put it. Nothing is moved.</p>
      <div><button class="btn" onclick={checkPaths} disabled={paths === 'running'}>{paths === 'running' ? 'Checking…' : 'Check'}</button></div>
      {#if paths && paths !== 'running'}
        {#if paths.error}<p class="result bad">{paths.error}</p>
        {:else}
          <p><b>{paths.matching}</b> files are exactly where Spool would put them{paths.differing.length ? `; ${paths.differing.length} are not:` : '.'}</p>
          {#each paths.differing.slice(0, 50) as d}<div class="mono small break diff"><div>on disk&nbsp;&nbsp; {d.actual}</div><div>expected {d.expected}</div></div>{/each}
        {/if}
      {/if}
    </fieldset>
    <p class="faint small">Spool {app.status?.version}. Movie details and artwork come from TMDB; series details come from TVmaze, or from TMDB for a series TVmaze does not list; anime formats and relations come from AniList. This product uses the TMDB API but is not endorsed or certified by TMDB.</p>
  </div>
