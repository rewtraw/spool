<script lang="ts">
  import { api } from '../lib/api';
  import { app, act, route, loadStatus, loadProfiles, toast } from '../lib/state.svelte';
  import { ago, bytes } from '../lib/format';

  const tabs = [['general', 'General'], ['usenet', 'Usenet'], ['indexers', 'Indexers'], ['profiles', 'Profiles'], ['naming', 'Naming'], ['plex', 'Plex'], ['space', 'Disk space'], ['maintenance', 'Maintenance']];
  const tab = $derived(route.arg || 'general');

  let general = $state<any>(null);
  let naming = $state<any>(null);
  let servers = $state<any[]>([]);
  let speedLimit = $state(0);
  let indexers = $state<any[]>([]);
  let profiles = $state<any[]>([]);
  let qualities = $state<any[]>([]);
  let tests = $state<Record<string, { ok: boolean; message: string } | 'running'>>({});
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
  let confirmActive = $state(false);

  // ---- AI access
  const mcpUrl = `${location.origin}/mcp`;
  function newKey() {
    return Array.from(crypto.getRandomValues(new Uint8Array(24)), (b) => b.toString(16).padStart(2, '0')).join('');
  }
  async function copyMcp(key: string) {
    const cmd = `claude mcp add --transport http --scope user spool ${mcpUrl} --header "Authorization: Bearer ${key}"`;
    try {
      await navigator.clipboard.writeText(cmd);
      toast('Copied. Paste it into a terminal.');
    } catch {
      prompt('Copy this command:', cmd);
    }
  }

  // ---- disk space
  let space = $state<any>(null);
  let spaceShow = $state<'all' | 'watched' | 'again'>('all');
  const spaceRows = $derived((space?.titles ?? []).filter((t: any) => (spaceShow === 'watched' ? t.watched : spaceShow === 'again' ? t.can_download_again : true)));
  async function loadSpace() {
    space = await api.get('/space').catch(() => space);
  }
  // ---- several titles at once
  let spacePicked = $state<Record<number, boolean>>({});
  const spaceChosen = $derived(spaceRows.filter((t: any) => spacePicked[t.title_id]));
  const spaceChosenBytes = $derived(spaceChosen.reduce((s: number, t: any) => s + t.size, 0));
  const spaceAllPicked = $derived(spaceRows.length > 0 && spaceRows.slice(0, 80).every((t: any) => spacePicked[t.title_id]));
  function spacePickAll() {
    const on = !spaceAllPicked;
    spacePicked = {};
    if (on) for (const t of spaceRows.slice(0, 80)) spacePicked[t.title_id] = true;
  }
  async function freeChosen() {
    const n = spaceChosen.length;
    const lost = spaceChosen.filter((t: any) => !t.can_download_again).length;
    const note = lost ? `${lost} of them ${lost === 1 ? 'has' : 'have'} no saved release, so getting ${lost === 1 ? 'it' : 'them'} back would need a new search.` : 'Each has a saved release, so it can be downloaded again later.';
    if (!confirm(`Delete the files of ${n} ${n === 1 ? 'title' : 'titles'} (${bytes(spaceChosenBytes)})? ${note} They stay in the library, unmonitored.`)) return;
    const r = await act(() => api.post<any>('/titles/bulk', { ids: spaceChosen.map((t: any) => t.title_id), action: 'free' }));
    if (r) toast(`Freed ${bytes(r.bytes)} from ${r.done} ${r.done === 1 ? 'title' : 'titles'}`);
    spacePicked = {};
    loadSpace();
    loadStatus();
  }
  async function compactChosen() {
    const ids = spaceChosen.filter((t: any) => t.over_target > 0).map((t: any) => t.title_id);
    if (!ids.length) return toast('None of the selected titles is over its size target');
    const r = await act(() => api.post<any>('/titles/bulk', { ids, action: 'compact' }));
    if (r) toast(`Looking for smaller copies of ${r.started} ${r.started === 1 ? 'title' : 'titles'} in the background. Downloads appear in Activity.`);
    spacePicked = {};
  }

  async function freeTitle(t: any) {
    const note = t.can_download_again ? 'A saved release is kept, so it can be downloaded again later.' : 'No saved release is kept for it; getting it back would need a new search.';
    if (!confirm(`Delete the ${t.files === 1 ? 'file' : `${t.files} files`} of ${t.title} (${bytes(t.size)})? ${note} The title stays in the library, unmonitored.`)) return;
    const r = await act(() => api.post<any>(`/titles/${t.title_id}/free`));
    if (r) toast(r.recycled ? `${bytes(r.bytes)} moved to the recycle folder` : `Freed ${bytes(r.bytes)}`);
    loadSpace();
    loadStatus();
  }
  const overTarget = $derived.by(() => {
    const rows = (space?.titles ?? []).filter((t: any) => t.over_target > 0);
    return { count: rows.length, bytes: rows.reduce((s: number, t: any) => s + t.over_target, 0) };
  });
  async function compactLibrary() {
    await act(() => api.post('/tasks/compact'), 'Looking for smaller copies of the titles furthest over target. Downloads appear in Activity.');
  }
  async function signOut() {
    await api.post('/logout').catch(() => {});
    location.reload();
  }
  async function compactTitle(t: any) {
    const r = await act(() => api.post<any>(`/titles/${t.title_id}/search`, { compact: true, grab: true }));
    if (r) toast(r.grabbed?.length ? `Downloading a smaller copy of ${t.title}; it replaces the file when it arrives` : r.message.startsWith('Nothing') ? r.message : `No smaller copy of ${t.title} that fits was found`);
    loadSpace();
  }
  async function emptyFromSpace() {
    if (!confirm(`Permanently delete ${space.recycled.files} recycled ${space.recycled.files === 1 ? 'file' : 'files'} (${bytes(space.recycled.bytes)})?`)) return;
    const r = await act(() => api.post<any>('/recycle/empty'));
    if (r) toast(`Freed ${bytes(r.freed)}`);
    loadSpace();
    loadStatus();
  }

  // ---- plex
  let plex = $state<any>(null);
  let plexBusy = $state(false);
  async function loadPlex() {
    plex = await api.get('/plex').catch(() => null);
  }
  async function syncPlex() {
    plexBusy = true;
    const r = await act(() => api.post<any>('/plex/sync'));
    plexBusy = false;
    if (r) toast(r.message);
    loadPlex();
  }
  async function trackPlex(item: any) {
    const t = await act(() => api.post<any>('/plex/track', { rating_key: item.rating_key }), `Now tracking ${item.title}`);
    if (t) loadPlex();
  }
  async function rescan(item: any) {
    await act(() => api.post(`/titles/${item.title_id}/scan`), 'Rescanned');
    loadPlex();
  }

  async function load() {
    if (tab === 'general') general = await api.get('/settings/general');
    if (tab === 'naming') naming = await api.get('/settings/naming');
    if (tab === 'usenet') {
      const r = await api.get('/settings/servers');
      servers = r.servers;
      speedLimit = Math.round(r.speed_limit / 1024 / 1024);
    }
    if (tab === 'indexers') indexers = await api.get('/indexers');
    if (tab === 'maintenance') loadRecycle();
    if (tab === 'plex') loadPlex();
    if (tab === 'space') loadSpace();
    if (tab === 'profiles') {
      profiles = await api.get('/profiles');
      qualities = await api.get('/qualities');
      await loadLimits();
    }
  }
  $effect(() => {
    tab;
    load().catch(() => {});
  });

  async function saveGeneral() {
    if (general.mode === 'active' && app.status?.mode !== 'active' && !confirmActive) {
      confirmActive = true;
      return;
    }
    confirmActive = false;
    await act(() => api.put('/settings/general', { ...general, recycle_days: Number(general.recycle_days), retention_days: Number(general.retention_days), rss_interval_minutes: Number(general.rss_interval_minutes), backlog_batch: Number(general.backlog_batch), backlog_interval_minutes: Number(general.backlog_interval_minutes), min_free_gb: Number(general.min_free_gb), post_parallel: Number(general.post_parallel), space_wait_gb: Number(general.space_wait_gb), archive_runner_ups: Number(general.archive_runner_ups), default_movie_profile: Number(general.default_movie_profile), default_series_profile: Number(general.default_series_profile) }), 'Saved');
    loadStatus();
    load();
  }

  // ---- usenet
  function addServer() {
    servers.push({ id: '', name: '', host: '', port: 563, tls: true, tls_verify: true, username: '', password: '', connections: 20, priority: servers.length, enabled: true, pipeline: 2 });
  }
  async function saveServers() {
    await act(() => api.put('/settings/servers', { servers: servers.map((s) => ({ ...s, port: Number(s.port), connections: Number(s.connections), priority: Number(s.priority), pipeline: Number(s.pipeline) })), speed_limit: Math.max(0, Number(speedLimit)) * 1024 * 1024 }), 'Saved');
    loadStatus();
    load();
  }
  async function testServer(s: any, i: number) {
    tests[`s${i}`] = 'running';
    tests[`s${i}`] = await api.post('/servers/test', { ...s, port: Number(s.port), connections: Number(s.connections), priority: Number(s.priority), pipeline: Number(s.pipeline) }).catch((e) => ({ ok: false, message: e.message }));
  }

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

  // ---- profiles
  const qname = (key: string, kind: string) => qualities.find((q) => q.key === key)?.[kind === 'movie' ? 'movie' : 'tv'] ?? key;
  function newProfile(kind: string) {
    const items = [...qualities].sort((a, b) => a.weight - b.weight).map((q) => ({ name: null, qualities: [q.key], allowed: ['bluray-1080p', 'webdl-1080p', 'webrip-1080p', 'hdtv-1080p'].includes(q.key) }));
    profiles.push({ id: 0, name: 'New profile', kind, upgrade_allowed: true, cutoff: 'bluray-1080p', items, target_size_gb: null, prefer_efficient_codec: false, prefer_direct_play: false });
  }
  function move(p: any, i: number, d: number) {
    const j = i + d;
    if (j < 0 || j >= p.items.length) return;
    [p.items[i], p.items[j]] = [p.items[j], p.items[i]];
  }
  async function saveProfile(p: any, i: number) {
    const allowed = p.items.filter((x: any) => x.allowed);
    if (allowed.length && !allowed.some((x: any) => x.qualities.includes(p.cutoff))) p.cutoff = allowed[allowed.length - 1].qualities[0];
    if (p.target_size_gb) p.target_size_gb = [Number(p.target_size_gb[0]) || 0, Math.max(1, Number(p.target_size_gb[1]) || 1)];
    const saved = await act(() => (p.id ? api.put(`/profiles/${p.id}`, p) : api.post('/profiles', p)), 'Saved');
    if (saved) profiles[i] = saved;
    loadProfiles();
  }
  async function removeProfile(p: any, i: number) {
    if (p.id) {
      const ok = await act(() => api.del(`/profiles/${p.id}`), 'Removed');
      if (!ok) return;
    }
    profiles.splice(i, 1);
    loadProfiles();
  }
  let editing = $state<number | null>(null);

  // ---- size limits (megabytes per minute of runtime; blank means no limit)
  let limits = $state<Record<string, { min: any; max: any }>>({});
  let showLimits = $state(false);
  async function loadLimits() {
    const raw = await api.get('/settings/size-limits');
    const out: Record<string, { min: any; max: any }> = {};
    // Every quality gets a row for both kinds, so each box has something to bind to.
    for (const q of qualities) for (const kind of ['movie', 'tv']) out[`${kind}:${q.key}`] = { min: '', max: '' };
    for (const [k, v] of Object.entries<any>(raw)) out[k] = { min: v.min || '', max: v.max ?? '' };
    limits = out;
  }
  async function saveLimits() {
    const body: Record<string, any> = {};
    for (const [k, v] of Object.entries(limits)) {
      const min = Number(v.min) || 0;
      const max = v.max === '' || v.max == null ? null : Number(v.max) || null;
      if (min > 0 || max) body[k] = { min, max, preferred: null };
    }
    await act(() => api.put('/settings/size-limits', body), 'Saved');
    loadLimits();
  }
  const gbFor = (mbPerMin: any, minutes: number) => (Number(mbPerMin) > 0 ? `${((Number(mbPerMin) * minutes) / 1024).toFixed(1)} GB` : '');

  // ---- maintenance
  const TASKS = [
    ['rss', 'Read indexer feeds', 'Checks every indexer for new releases the library wants.'],
    ['backlog', 'Search for missing titles', 'Searches a few missing titles, least recently searched first.'],
    ['refresh', 'Refresh details', 'Updates titles and episode lists from TMDB and TVmaze.'],
    ['scan', 'Rescan library folders', 'Finds files added or removed outside Spool. Changes the catalog only.'],
    ['housekeeping', 'Back up and tidy', 'Writes a database backup and empties old recycle folders.'],
    ['plex', 'Read Plex', 'Matches Plex\'s films and shows to titles in Spool.'],
    ['subtitles', 'Find missing subtitles', 'Looks for subtitles in the languages you chose for a few files that lack them, and keeps only ones in time with the film.'],
  ];
  async function runTask(name: string) {
    await act(() => api.post(`/tasks/${name}`), 'Started');
  }
  async function checkPaths() {
    paths = 'running';
    paths = await api.get('/check-paths').catch((e) => ({ error: e.message }));
  }
</script>

<header class="head"><h1>Settings</h1></header>
<div class="tabs" role="tablist">
  {#each tabs as [id, label]}<a role="tab" aria-selected={tab === id} class:on={tab === id} href="#/settings/{id}">{label}</a>{/each}
</div>

{#if tab === 'general' && general}
  <form class="stack form" onsubmit={(e) => { e.preventDefault(); saveGeneral(); }}>
    <fieldset class="card">
      <legend>Mode</legend>
      <label class="radio"><input type="radio" bind:group={general.mode} value="shadow" /><span><b>Shadow</b><small>Reads the feeds and records what it would do. Never downloads, moves or deletes.</small></span></label>
      <label class="radio"><input type="radio" bind:group={general.mode} value="active" /><span><b>Active</b><small>Downloads wanted releases and moves them into the library.</small></span></label>
      {#if confirmActive}
        <div class="confirm">
          <p><b>Make Spool active?</b> It will start downloading and writing to the library folders below. Stop Radarr and Sonarr from managing the same folders first, or two apps will fight over the same files.</p>
          <div class="row"><button class="btn primary" type="submit">Yes, make it active</button><button class="btn ghost" type="button" onclick={() => { confirmActive = false; general.mode = 'shadow'; }}>Stay in shadow</button></div>
        </div>
      {/if}
    </fieldset>

    <fieldset class="card">
      <legend>Folders</legend>
      <label class="field">Movies<input class="input mono" bind:value={general.movie_root} placeholder="/Volumes/Media/Movies" /></label>
      <label class="field">Series<input class="input mono" bind:value={general.series_root} placeholder="/Volumes/Media/TV Shows" /></label>
      <label class="field">Downloads<input class="input mono" bind:value={general.downloads_dir} placeholder="/Volumes/Media/downloads" /><span class="hint">Keep this on the same disk as the library so finished files are moved, not copied.</span></label>
      <label class="field">Disk that must be mounted<input class="input mono" bind:value={general.required_volume} placeholder="/Volumes/Media" /><span class="hint">If this disk is not mounted, Spool pauses instead of writing somewhere else. Leave empty to skip the check.</span></label>
      <label class="field narrow">Keep free on the downloads disk (GB)<input class="input" type="number" min="0" bind:value={general.min_free_gb} /><span class="hint">Spool does not start a download that would leave less than this free once it has finished unpacking.</span></label>
      <label class="field narrow">Let downloads wait for room, up to (GB)<input class="input" type="number" min="0" bind:value={general.space_wait_gb} /><span class="hint">When the disk is short, Spool still queues what it finds, up to this much beyond the free space, and holds it without downloading until there is room. 0 makes it skip them instead.</span></label>
      <label class="check"><input type="checkbox" bind:checked={general.direct_unpack} /> Unpack while downloading</label>
      <span class="hint">Posts packed as uncompressed RAR volumes are unpacked as each volume arrives and the volume is deleted once verified, so a download needs about half the room and no extraction step. Anything else is handled the usual way.</span>
      <label class="field narrow">Downloads checked and unpacked at once<input class="input" type="number" min="1" max="8" bind:value={general.post_parallel} /><span class="hint">Takes effect the next time Spool starts.</span></label>
      <label class="field narrow">Keep replaced and deleted files for (days)<input class="input" type="number" min="0" bind:value={general.recycle_days} /><span class="hint">They wait in a <code>.spool-recycle</code> folder in the library and are removed automatically after this long. 0 deletes them at once, with no way back.</span></label>
    </fieldset>

    <fieldset class="card">
      <legend>Choosing releases</legend>
      <label class="field narrow">Fallback releases to save per download<input class="input" type="number" min="0" max="5" bind:value={general.archive_runner_ups} /><span class="hint">When Spool picks a release it also saves this many of the next-best ones to the <a class="link" href="#/archive">Archive</a>. Each costs one indexer download. 0 keeps only what is downloaded.</span></label>
      <label class="field narrow">Language<input class="input" bind:value={general.language} placeholder="english" /><span class="hint">Releases that name a different language are rejected, unless it is the language the film or series was made in. Leave empty to accept any.</span></label>
      <label class="field narrow">Propers and repacks
        <select class="input" bind:value={general.proper_policy}>
          <option value="prefer_and_upgrade">Prefer them, and replace an existing file</option>
          <option value="do_not_upgrade">Prefer them, but never replace a file for one</option>
          <option value="do_not_prefer">Treat like any other release</option>
        </select>
      </label>
      <label class="field narrow">Usenet retention (days)<input class="input" type="number" min="0" bind:value={general.retention_days} /><span class="hint">Older posts are rejected. 0 means no limit.</span></label>
      <div class="cols">
        <label class="field">Profile for new movies
          <select class="input" bind:value={general.default_movie_profile}>
            {#each app.profiles.filter((p) => p.kind === 'movie') as p}<option value={p.id}>{p.name}</option>{/each}
          </select>
        </label>
        <label class="field">Profile for new series
          <select class="input" bind:value={general.default_series_profile}>
            {#each app.profiles.filter((p) => p.kind === 'tv') as p}<option value={p.id}>{p.name}</option>{/each}
          </select>
        </label>
      </div>
      <div class="cols">
        <label class="field">Read feeds every (minutes)<input class="input" type="number" min="10" bind:value={general.rss_interval_minutes} /></label>
        <label class="field">Search missing every (minutes)<input class="input" type="number" min="60" bind:value={general.backlog_interval_minutes} /></label>
        <label class="field">Titles per missing search<input class="input" type="number" min="0" bind:value={general.backlog_batch} /></label>
      </div>
    </fieldset>

    <fieldset class="card">
      <legend>Connections</legend>
      <label class="field">TMDB API key<input class="input mono" bind:value={general.tmdb_api_key} autocomplete="off" /><span class="hint">Needed to look up and add movies. Free from themoviedb.org.</span></label>
      <div class="cols">
        <label class="field">Plex address<input class="input mono" bind:value={general.plex_url} placeholder="http://127.0.0.1:32400" /></label>
        <label class="field">Plex token<input class="input mono" bind:value={general.plex_token} autocomplete="off" /></label>
      </div>
      <label class="field narrow">Password for this app<input class="input" type="password" bind:value={general.password} autocomplete="new-password" /><span class="hint">Empty means anyone who can reach this address can use it. Setting or changing it signs every other browser out.</span></label>
      {#if general.password}<div><button class="btn ghost" type="button" onclick={signOut}>Sign out of this browser</button></div>{/if}
      <label class="field">Second backup folder<input class="input mono" bind:value={general.backup_dir} autocomplete="off" placeholder="None" /><span class="hint">The nightly database backup is also copied here. Choose a folder on a different disk from the one Spool's data is on.</span></label>
    </fieldset>
    <fieldset class="card">
      <legend>Subtitles</legend>
      <p class="muted">Spool keeps the subtitle files that come with a release, and can look for more on OpenSubtitles. Every subtitle file it fetches is checked against the speech in the film: one that is out by a fixed amount is retimed, and one that does not fit is thrown away and the next tried.</p>
      <label class="field narrow">Languages wanted<input class="input" bind:value={general.subtitle_languages} placeholder="en" autocomplete="off" /><span class="hint">Codes or names, separated by commas: <code>en</code>, or <code>en, es</code>. A file with that language inside it or beside it needs nothing more.</span></label>
      <label class="field">OpenSubtitles API key<input class="input mono" bind:value={general.opensubtitles_api_key} autocomplete="off" /><span class="hint">Free from opensubtitles.com, under API consumers in your profile.</span></label>
      <div class="cols">
        <label class="field">OpenSubtitles username<input class="input" bind:value={general.opensubtitles_username} autocomplete="off" /></label>
        <label class="field">OpenSubtitles password<input class="input" type="password" bind:value={general.opensubtitles_password} autocomplete="new-password" /></label>
      </div>
      <label class="check"><input type="checkbox" bind:checked={general.subtitles_auto} /> Look for missing subtitles automatically</label>
      <span class="hint">A few files every six hours, newest first, to stay inside the account's daily download allowance. Each file is tried again after a week.</span>
    </fieldset>
    <fieldset class="card">
      <legend>AI access</legend>
      <p class="muted">Spool speaks the Model Context Protocol at <code>{mcpUrl}</code>, so an AI session can look things up, add and remove titles, start and manage downloads, read the log and work with Plex. Deleting files always needs an explicit confirmation.</p>
      <div class="row wrap">
        <button class="btn" type="button" onclick={() => copyMcp(general.api_key)} disabled={!general.api_key}>Copy the Claude Code command (full access)</button>
        {#if general.mcp_read_key}<button class="btn ghost" type="button" onclick={() => copyMcp(general.mcp_read_key)}>Copy it with the read-only key</button>{/if}
      </div>
      <label class="field">Read-only key
        <div class="row"><input class="input mono grow" bind:value={general.mcp_read_key} autocomplete="off" placeholder="None" /><button class="btn" type="button" onclick={() => (general.mcp_read_key = newKey())}>Generate</button></div>
        <span class="hint">Optional. A session using this key can look but not change anything. Save after changing it; empty turns it off.</span>
      </label>
    </fieldset>
    <div><button class="btn primary" type="submit">Save</button></div>
  </form>
{:else if tab === 'usenet'}
  <div class="stack form">
    {#each servers as s, i}
      <fieldset class="card">
        <legend>{s.name || s.host || 'New server'}</legend>
        <div class="cols">
          <label class="field">Host<input class="input mono" bind:value={s.host} placeholder="news.example.com" /></label>
          <label class="field">Port<input class="input" type="number" bind:value={s.port} /></label>
          <label class="field">Connections<input class="input" type="number" min="1" max="100" bind:value={s.connections} /></label>
        </div>
        <div class="cols">
          <label class="field">Username<input class="input mono" bind:value={s.username} autocomplete="off" /></label>
          <label class="field">Password<input class="input mono" type="password" bind:value={s.password} autocomplete="new-password" /></label>
          <label class="field">Priority<input class="input" type="number" min="0" bind:value={s.priority} /><span class="hint">0 is asked first. Higher numbers only fill in missing articles.</span></label>
        </div>
        <div class="row wrap">
          <label class="check"><input type="checkbox" bind:checked={s.enabled} /> Enabled</label>
          <label class="check"><input type="checkbox" bind:checked={s.tls} /> TLS</label>
          <label class="check"><input type="checkbox" bind:checked={s.tls_verify} disabled={!s.tls} /> Verify certificate</label>
          <span class="grow"></span>
          {#if tests[`s${i}`]}<span class="result" class:bad={tests[`s${i}`] !== 'running' && !(tests[`s${i}`] as any).ok}>{tests[`s${i}`] === 'running' ? 'Testing…' : (tests[`s${i}`] as any).message}</span>{/if}
          <button class="btn small" onclick={() => testServer(s, i)}>Test</button>
          <button class="btn small ghost danger" onclick={() => servers.splice(i, 1)}>Remove</button>
        </div>
      </fieldset>
    {/each}
    {#if !servers.length}<div class="card empty">No usenet server yet. Spool cannot download without one.</div>{/if}
    <fieldset class="card">
      <legend>Speed</legend>
      <label class="field narrow">Limit (MB per second)<input class="input" type="number" min="0" bind:value={speedLimit} /><span class="hint">0 means no limit.</span></label>
    </fieldset>
    <div class="row"><button class="btn" onclick={addServer}>Add a server</button><button class="btn primary" onclick={saveServers}>Save</button></div>
  </div>
{:else if tab === 'indexers'}
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
{:else if tab === 'profiles'}
  <div class="stack form">
    <p class="muted">A profile lists the qualities a title may be downloaded in, worst to best. Spool takes the best one it can find and stops upgrading once it reaches the cutoff.</p>
    {#each profiles as p, i}
      <fieldset class="card">
        <legend>{p.name} <span class="chip">{p.kind === 'movie' ? 'Movies' : 'Series'}</span></legend>
        {#if editing === i}
          <div class="cols">
            <label class="field">Name<input class="input" bind:value={p.name} /></label>
            <label class="field">Stop upgrading at
              <select class="input" bind:value={p.cutoff}>
                {#each p.items.filter((x: any) => x.allowed) as it}<option value={it.qualities[0]}>{it.name ?? qname(it.qualities[0], p.kind)}</option>{/each}
              </select>
            </label>
          </div>
          <label class="check"><input type="checkbox" bind:checked={p.upgrade_allowed} /> Replace a file when a better quality appears</label>
          <label class="check"><input type="checkbox" bind:checked={p.prefer_direct_play} /> Prefer what an Apple TV plays without conversion</label>
          {#if p.prefer_direct_play}<p class="hint">Between releases of the same quality: H.265 video first, then Dolby Vision that also carries HDR10, then Dolby Digital Plus audio over TrueHD and DTS. AV1 is used only when nothing else is acceptable.</p>
          {:else}<label class="check"><input type="checkbox" bind:checked={p.prefer_efficient_codec} /> Between equal releases, prefer H.265 or AV1</label>{/if}
          <label class="check"><input type="checkbox" checked={!!p.target_size_gb} onchange={(e) => (p.target_size_gb = e.currentTarget.checked ? [15, 25] : null)} /> Aim for a size</label>
          {#if p.target_size_gb}
            <div class="cols">
              <label class="field">From (GB)<input class="input" type="number" min="0" step="1" bind:value={p.target_size_gb[0]} /></label>
              <label class="field">To (GB)<input class="input" type="number" min="1" step="1" bind:value={p.target_size_gb[1]} /></label>
            </div>
            <p class="hint">A preference, not a filter. Releases that fit are chosen first, best quality leading. A bigger one is taken only when nothing fits, and never to replace a file you already have.{p.kind === 'tv' ? ' For series the size is per episode.' : ''}</p>
          {/if}
          <div class="qlist">
            {#each [...p.items].reverse() as it, r}
              {@const idx = p.items.length - 1 - r}
              <div class="q" class:off={!it.allowed}>
                <input type="checkbox" bind:checked={it.allowed} aria-label="Allow" />
                <span class="grow">{it.name ? `${it.name}: ` : ''}{it.qualities.map((k: string) => qname(k, p.kind)).join(', ')}</span>
                <button class="btn small ghost" onclick={() => move(p, idx, 1)} disabled={idx === p.items.length - 1} aria-label="Move up">↑</button>
                <button class="btn small ghost" onclick={() => move(p, idx, -1)} disabled={idx === 0} aria-label="Move down">↓</button>
              </div>
            {/each}
          </div>
          <div class="row"><button class="btn primary" onclick={async () => { await saveProfile(p, i); editing = null; }}>Save</button><button class="btn ghost" onclick={() => { editing = null; load(); }}>Cancel</button><span class="grow"></span><button class="btn ghost danger" onclick={() => { editing = null; removeProfile(p, i); }}>Remove</button></div>
        {:else}
          <div class="row wrap">
            <span class="grow muted">{p.items.filter((x: any) => x.allowed).map((x: any) => x.name ?? qname(x.qualities[0], p.kind)).reverse().join(' › ') || 'Nothing allowed'}</span>
            <span class="faint small">{[p.upgrade_allowed ? `Upgrades until ${qname(p.cutoff, p.kind)}` : 'No upgrades', p.target_size_gb ? `aims for ${p.target_size_gb[0]} to ${p.target_size_gb[1]} GB` : '', p.prefer_direct_play ? 'prefers direct play on Apple TV' : p.prefer_efficient_codec ? 'prefers H.265' : ''].filter(Boolean).join(' · ')}</span>
            <button class="btn small" onclick={() => (editing = i)}>Edit</button>
          </div>
        {/if}
      </fieldset>
    {/each}
    <div class="row"><button class="btn" onclick={() => { newProfile('movie'); editing = profiles.length - 1; }}>New movie profile</button><button class="btn" onclick={() => { newProfile('tv'); editing = profiles.length - 1; }}>New series profile</button></div>

    <fieldset class="card">
      <legend>Hard size limits</legend>
      <p class="muted">These reject a release outright, whatever its profile, when it is smaller or larger than a quality should be. They are in megabytes per minute of runtime, so they scale with length. Leave a box empty for no limit. To steer toward a size without rejecting anything, use a profile's size target instead.</p>
      <div><button class="btn small" onclick={() => (showLimits = !showLimits)}>{showLimits ? 'Hide' : 'Show'} limits</button></div>
      {#if showLimits}
        <div class="scroll-x">
          <table class="table limits">
            <thead>
              <tr><th></th><th colspan="2">Movies</th><th colspan="2">Series</th></tr>
              <tr><th>Quality</th><th>At least</th><th>At most</th><th>At least</th><th>At most</th></tr>
            </thead>
            <tbody>
              {#each [...qualities].sort((a, b) => b.weight - a.weight) as q (q.key)}
                {#if limits[`movie:${q.key}`] && limits[`tv:${q.key}`]}
                  <tr>
                    <td>{q.movie}</td>
                    <td><input class="input" type="number" min="0" bind:value={limits[`movie:${q.key}`].min} /></td>
                    <td><input class="input" type="number" min="0" bind:value={limits[`movie:${q.key}`].max} title={gbFor(limits[`movie:${q.key}`].max, 120) ? `${gbFor(limits[`movie:${q.key}`].max, 120)} for a two-hour film` : ''} /></td>
                    <td><input class="input" type="number" min="0" bind:value={limits[`tv:${q.key}`].min} /></td>
                    <td><input class="input" type="number" min="0" bind:value={limits[`tv:${q.key}`].max} title={gbFor(limits[`tv:${q.key}`].max, 45) ? `${gbFor(limits[`tv:${q.key}`].max, 45)} for a 45-minute episode` : ''} /></td>
                  </tr>
                {/if}
              {/each}
            </tbody>
          </table>
        </div>
        <div><button class="btn primary" onclick={saveLimits}>Save limits</button></div>
      {/if}
    </fieldset>
  </div>
{:else if tab === 'naming' && naming}
  <form class="stack form" onsubmit={async (e) => { e.preventDefault(); await act(() => api.put('/settings/naming', naming), 'Saved'); }}>
    <fieldset class="card">
      <legend>Renaming</legend>
      <label class="check"><input type="checkbox" bind:checked={naming.rename} /> Rename files when importing</label>
      <label class="check"><input type="checkbox" bind:checked={naming.replace_illegal_characters} /> Replace characters a file name cannot hold</label>
      <p class="hint">These formats use the same tokens as Radarr and Sonarr, so existing files keep their names.</p>
    </fieldset>
    <fieldset class="card">
      <legend>Movies</legend>
      <label class="field">Folder<input class="input mono" bind:value={naming.movie_folder_format} /></label>
      <label class="field">File<textarea class="input" rows="3" bind:value={naming.movie_file_format}></textarea></label>
    </fieldset>
    <fieldset class="card">
      <legend>Series</legend>
      <label class="field">Series folder<input class="input mono" bind:value={naming.series_folder_format} /></label>
      <label class="field">Season folder<input class="input mono" bind:value={naming.season_folder_format} /></label>
      <label class="field">Episode file<textarea class="input" rows="2" bind:value={naming.episode_file_format}></textarea></label>
    </fieldset>
    <div><button class="btn primary" type="submit">Save</button></div>
  </form>
{:else if tab === 'plex'}
  <div class="stack form">
    {#if plex && !plex.configured}
      <div class="card empty">Plex is not set up. Add its address and token under <a class="link" href="#/settings/general">General</a>.</div>
    {:else if plex}
      <fieldset class="card">
        <legend>Library match</legend>
        <p class="muted">
          {#if plex.synced_at}Plex was last read {ago(plex.synced_at)}: <b>{plex.plex_items}</b> films and shows, <b>{plex.matched}</b> matched to titles in Spool.{:else}Plex has not been read yet.{/if}
          Matched titles get an Open in Plex link on their page. Spool reads Plex once an hour.
        </p>
        <div><button class="btn" disabled={plexBusy} onclick={syncPlex}>{plexBusy ? 'Reading Plex…' : 'Read Plex now'}</button></div>
      </fieldset>
      <fieldset class="card">
        <legend>In Plex, not in Spool ({plex.plex_only.length})</legend>
        {#if !plex.plex_only.length}<p class="muted">Everything in Plex is tracked by Spool.</p>
        {:else}
          <p class="muted">Tracking adds the title to Spool unmonitored, pointing at the folder Plex plays it from. Nothing is moved, renamed or downloaded.</p>
          <div class="list">
            {#each plex.plex_only as it (it.rating_key)}
              <div class="task">
                <div class="grow"><b>{it.title}</b> <span class="muted">{it.year || ''}</span><div class="faint small">{it.section}{it.path ? ` · ${it.path}` : ''}</div></div>
                {#if it.url}<a class="btn small ghost" href={it.url} target="_blank" rel="noopener noreferrer">Open in Plex</a>{/if}
                {#if it.can_track}<button class="btn small" onclick={() => trackPlex(it)}>Track in Spool</button>{:else}<span class="faint small">Plex has no id for it</span>{/if}
              </div>
            {/each}
          </div>
        {/if}
      </fieldset>
      <fieldset class="card">
        <legend>In Spool with files, not in Plex ({plex.spool_only.length})</legend>
        {#if !plex.spool_only.length}<p class="muted">Every title with files was found in Plex.</p>
        {:else}
          <p class="muted">Plex may not have scanned these yet, may have matched them to the wrong film, or the files may be gone. Rescanning checks the folder on disk.</p>
          <div class="list">
            {#each plex.spool_only as it (it.title_id)}
              <div class="task">
                <div class="grow"><a class="link" href="#/title/{it.title_id}"><b>{it.title}</b></a> <span class="muted">{it.year || ''}</span><div class="faint small break">{it.path}</div></div>
                <button class="btn small" onclick={() => rescan(it)}>Rescan folder</button>
              </div>
            {/each}
          </div>
        {/if}
      </fieldset>
    {/if}
  </div>
{:else if tab === 'space'}
  <div class="stack form">
    {#if space}
      <div class="card figures">
        <div><b>{bytes(space.free)}</b><span>Free</span></div>
        <div><b>{bytes(space.library)}</b><span>Library</span></div>
        <div><b>{bytes(space.recycled.bytes)}</b><span>Recycle folder</span></div>
        <div><b>{space.waiting.count ? bytes(space.waiting.bytes) : 'None'}</b><span>Waiting for room</span></div>
      </div>
      {#if space.waiting.count}
        <div class="notice warn">{space.waiting.count} {space.waiting.count === 1 ? 'download is' : 'downloads are'} held until there is room. Spool keeps {bytes(space.kept_free)} free and starts them by itself as space appears.</div>
      {/if}
      {#if space.recycled.files}
        <fieldset class="card">
          <legend>Recycle folder</legend>
          <p class="muted">{space.recycled.files} replaced or deleted {space.recycled.files === 1 ? 'file is' : 'files are'} still on disk, using {bytes(space.recycled.bytes)}. {#if space.recycled.days > 0}They are removed after {space.recycled.days} {space.recycled.days === 1 ? 'day' : 'days'}.{/if}</p>
          <div><button class="btn" onclick={emptyFromSpace}>Delete them now</button></div>
        </fieldset>
      {/if}
      {#if overTarget.count}
        <fieldset class="card">
          <legend>Over their size target</legend>
          <p class="muted">{overTarget.count} {overTarget.count === 1 ? 'title is' : 'titles are'} larger than the profile asks for, by {bytes(overTarget.bytes)} in all. Spool can look for smaller copies of the worst few at a time, accepting lower quality, and replace each file when its smaller copy arrives.</p>
          <div><button class="btn" onclick={compactLibrary}>Find smaller copies</button></div>
        </fieldset>
      {/if}
      <div class="row wrap">
        <div class="section-title grow" style="margin:0">Largest titles</div>
        <select class="input" style="width:auto" bind:value={spaceShow} aria-label="Show">
          <option value="all">All</option>
          <option value="watched">Watched in Plex</option>
          <option value="again">Can be downloaded again</option>
        </select>
      </div>
      {#if !spaceRows.length}
        <div class="card empty">Nothing matches.</div>
      {:else}
        <div class="card scroll-x">
          <table class="table">
            <thead><tr><th style="width:1%"><input class="tick" type="checkbox" aria-label="Select all shown" checked={spaceAllPicked} onchange={spacePickAll} /></th><th>Title</th><th>Size</th><th>Plex</th><th>Saved release</th><th></th></tr></thead>
            <tbody>
              {#each spaceRows.slice(0, 80) as t (t.title_id)}
                <tr>
                  <td><input class="tick" type="checkbox" aria-label="Select {t.title}" bind:checked={spacePicked[t.title_id]} /></td>
                  <td><a class="link" href="#/title/{t.title_id}"><b>{t.title}</b></a> <span class="muted">{t.year || ''}</span><div class="faint small">{t.kind === 'movie' ? 'Film' : `Series · ${t.files} files`}{t.monitored ? '' : ' · not monitored'}</div></td>
                  <td style="white-space:nowrap"><b>{bytes(t.size)}</b>{#if t.over_target > 0}<div class="faint small">{bytes(t.over_target)} over target</div>{/if}</td>
                  <td style="white-space:nowrap">
                    {#if !t.in_plex}<span class="faint">Not in Plex</span>
                    {:else if t.watched}<span class="chip ok">Watched</span>{#if t.last_viewed_at}<div class="faint small">{ago(t.last_viewed_at)}</div>{/if}
                    {:else if t.watched_count}<span class="chip">{t.watched_count} of {t.plex_items} watched</span>
                    {:else}<span class="muted">Not watched</span>{/if}
                  </td>
                  <td style="white-space:nowrap">{#if t.can_download_again}<span class="chip ok">Kept</span>{:else if t.saved_releases}<span class="chip bad">Gone from Usenet</span>{:else}<span class="faint">None</span>{/if}</td>
                  <td><div class="row" style="gap:6px;flex-wrap:nowrap">{#if t.over_target > 0}<button class="btn small" onclick={() => compactTitle(t)}>Find smaller</button>{/if}<button class="btn small ghost" onclick={() => freeTitle(t)}>Delete files</button></div></td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
        {#if spaceChosen.length}
          <div class="bulkbar" role="toolbar" aria-label="Actions for selected titles">
            <span class="n">{spaceChosen.length} selected · {bytes(spaceChosenBytes)}</span>
            <button class="btn small ghost" onclick={() => (spacePicked = {})}>Select none</button>
            <span class="sep"></span>
            <button class="btn small" onclick={compactChosen}>Find smaller copies</button>
            <button class="btn small ghost danger" onclick={freeChosen}>Delete files</button>
          </div>
        {/if}
        <p class="faint small">"Find smaller" looks for a copy that fits the title's profile size target and is at least a fifth smaller, accepting lower quality, and replaces the file when it arrives. Deleting files keeps the title in the library, unmonitored, so Spool does not fetch it again. "Kept" means its release is in the <a class="link" href="#/archive">Archive</a> and one click brings it back.</p>
      {/if}
    {/if}
  </div>
{:else if tab === 'maintenance'}
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
    <p class="faint small">Spool {app.status?.version}. Movie details and artwork come from TMDB; series details come from TVmaze. This product uses the TMDB API but is not endorsed or certified by TMDB.</p>
  </div>
{/if}

<style>
  .usage {
    padding: 8px 0 2px;
    font-size: 14.5px;
  }
  .figures {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(130px, 1fr));
    gap: 18px 28px;
    padding: 18px 22px;
  }
  .figures div {
    display: grid;
    gap: 3px;
    min-width: 0;
  }
  .figures b {
    font: 600 22px/1.2 var(--serif);
    font-variant-numeric: tabular-nums;
  }
  .figures span {
    color: var(--muted);
    font-size: 12.5px;
  }
  .head {
    margin-bottom: 16px;
  }
  .tabs {
    display: flex;
    gap: 22px;
    border-bottom: 1px solid var(--line);
    margin-bottom: 22px;
    overflow-x: auto;
  }
  .tabs a {
    padding: 9px 0;
    color: var(--muted);
    font-weight: 550;
    border-bottom: 2px solid transparent;
    margin-bottom: -1px;
    white-space: nowrap;
  }
  .tabs a.on {
    color: var(--text);
    border-color: var(--accent);
  }
  .form {
    max-width: 860px;
  }
  fieldset {
    margin: 0;
    padding: 18px;
    display: grid;
    gap: 14px;
    min-width: 0;
  }
  legend {
    font: 600 17px var(--serif);
    padding: 0 6px;
    margin-left: -6px;
  }
  .cols {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(190px, 1fr));
    gap: 12px;
  }
  .narrow {
    max-width: 420px;
  }
  .hint {
    font-size: 12.5px;
    color: var(--faint);
  }
  .radio {
    display: flex;
    gap: 11px;
    align-items: flex-start;
    cursor: pointer;
  }
  .radio input {
    margin-top: 4px;
    accent-color: var(--accent);
  }
  .radio span {
    display: grid;
  }
  .radio small {
    color: var(--muted);
    font-size: 13.5px;
  }
  .confirm {
    padding: 14px;
    border-radius: 8px;
    background: color-mix(in srgb, var(--warn) 14%, transparent);
    display: grid;
    gap: 12px;
  }
  .result {
    font-size: 13.5px;
    color: var(--ok);
  }
  .result.bad {
    color: var(--bad);
  }
  .qlist {
    border: 1px solid var(--line);
    border-radius: 8px;
    overflow: hidden;
  }
  .q {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 5px 8px 5px 12px;
    border-bottom: 1px solid var(--line);
    font-size: 14px;
  }
  .q:last-child {
    border-bottom: none;
  }
  .q.off span {
    color: var(--faint);
  }
  .list {
    overflow: hidden;
  }
  .task {
    display: flex;
    gap: 14px;
    align-items: center;
    padding: 13px 16px;
    border-bottom: 1px solid var(--line);
  }
  .task:last-child {
    border-bottom: none;
  }
  .small {
    font-size: 12.5px;
  }
  .limits td {
    padding: 4px 8px;
    vertical-align: middle;
  }
  .limits .input {
    width: 92px;
    height: 30px;
  }
  .link {
    color: var(--accent);
    font-weight: 550;
  }
  code {
    font-family: var(--mono);
    font-size: 12px;
  }
  .diff {
    padding: 8px 0;
    border-top: 1px solid var(--line);
  }
</style>
