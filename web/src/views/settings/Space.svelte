<script lang="ts">
  // The Disk space tab: what is using the disk, and what could go.
  import { api } from '../../lib/api';
  import { act, loadStatus, toast } from '../../lib/state.svelte';
  import { ago, bytes } from '../../lib/format';

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

  loadSpace();
</script>

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
