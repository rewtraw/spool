<script lang="ts">
  // The Library tab: each machine that holds some of the library, and the locations on it.
  import { api } from '../../lib/api';
  import { app, act, loadStatus, toast } from '../../lib/state.svelte';
  import { bytes } from '../../lib/format';

  let general = $state<any>(null);

  // ---- further locations, and bringing in what is already on them
  let places = $state<any[]>([]);
  // What was found on each location, by its id: kept by Spool, so it is still here after
  // looking through another, or coming back another day.
  let foundBy = $state<Record<number, any>>({});
  let picks = $state<Record<string, any>>({});
  let monitorAdopted = $state(false);
  let foundTimer: ReturnType<typeof setTimeout> | undefined;
  const working = $derived(Object.values(foundBy).some((f: any) => f?.running));
  async function loadPlaces() {
    places = await api.get<any[]>('/locations').catch(() => []);
    for (const p of places.filter((p) => p.id)) {
      const last = await api.get<any>(`/locations/${p.id}/found`).catch(() => null);
      if (last) {
        foundBy[p.id] = last;
        if (last.running) watchFound(p.id);
      }
    }
  }
  const placeOf = (id: number) => places.find((p) => p.id === id);
  // Trying a location out: can it be listed, read and, where Spool should, written.
  let checks = $state<Record<number, any>>({});
  async function checkPlace(id: number) {
    checks[id] = 'running';
    checks[id] = await api.post(`/locations/${id}/check`).catch((e) => ({ ok: false, checks: [{ what: 'check', ok: false, detail: e.message }] }));
  }
  // ---- linked Spools: today a helper, a small Spool on the machine that holds some of the disks
  let links = $state<any[]>([]);
  let linkTimer: ReturnType<typeof setInterval> | undefined;
  async function loadLinks() {
    links = await api.get<any[]>('/links').catch(() => links);
  }
  const linkOf = (id: number) => links.find((h) => h.id === id);
  // Linking with the code the other Spool gave, in place of copying a key.
  let codes = $state<Record<number, string>>({});
  let linking = $state<number | null>(null);
  async function linkWithCode(h: any, i: number) {
    // Once the other side has taken the key it takes no more codes, so nothing that
    // would stop the settings being saved is left to find out afterwards.
    if (!h.name?.trim() || !h.url?.trim()) {
      toast('Give it a name and an address first.', 'error');
      return;
    }
    linking = i;
    const got = await act(() => api.post<{ key: string }>('/links/pair', { url: h.url, code: codes[i] }));
    linking = null;
    if (!got) return;
    h.key = got.key;
    codes[i] = '';
    if (await save()) toast(`Linked with ${h.name}.`);
    else toast(`Linked with ${h.name}, but the settings were not saved. Put right what is wrong and save: the key is kept on this page until you do.`, 'error');
  }
  function addLink(from?: { name: string; url: string }) {
    (general.links ??= []).push({ id: 0, name: from?.name ?? '', url: from?.url ?? '', key: '', folders: [], play_near: true, make: 'always' });
    if (from) quick.add(general.links.at(-1));
  }
  // Links begun from one that was found: its name and address are known, so the code is all there is to ask.
  const quick = new WeakSet<object>();
  // Spools heard of on the network that are not linked yet: addresses to try, no more.
  let found = $state<any[]>([]);
  const notAdded = $derived(found.filter((f) => !(general?.links ?? []).some((h: any) => (h.url ?? '').replace(/\/+$/, '') === f.url)));
  async function loadFound() {
    found = await api.get<any[]>('/links/found').catch(() => found);
  }
  async function lookAgain(h: any) {
    const got = await act(() => api.post<any[]>(`/links/${h.id}/find`), 'Looked');
    if (got) (links = got), load();
  }
  // What a linked Spool allows this one, in words.
  const allows = (seen: any) => {
    const words = ['see', 'play', 'work', 'make'].filter((g) => seen.grants?.[g]);
    return words.length ? `allows ${words.length > 1 ? `${words.slice(0, -1).join(', ')} and ${words.at(-1)}` : words[0]}` : '';
  };
  $effect(() => {
    loadLinks();
    loadFound();
    linkTimer = setInterval(() => (loadLinks(), loadFound()), 5000);
    return () => clearInterval(linkTimer);
  });
  // A location is on the machine whose disks its folders are mounted under.
  const under = (folder: string, root: string) => !!root && !!folder && (folder === root || folder.startsWith(root.replace(/\/+$/, '') + '/'));
  const linkFor = (l: any) => (general.links ?? []).find((h: any) => (h.folders ?? []).some((f: any) => under(l.movie_root, f.here) || under(l.series_root, f.here)));
  // The folders a linked Spool shares, as this machine sees them, to choose a location's from.
  const sharedHere = (h: any): string[] => (linkOf(h.id)?.seen.shares ?? []).map((sh: any) => (h.folders ?? []).find((f: any) => f.there === sh.path)?.here).filter(Boolean);
  const sharedBy = (l: any) => {
    const at = (general.links ?? []).indexOf(linkFor(l));
    return at < 0 ? undefined : `shared-${at}`;
  };
  // A location on a linked machine starts from the first folder that machine shares, so that it is listed under it.
  function addPlaceOn(h: any) {
    general.locations.push({ id: 0, name: '', movie_root: sharedHere(h)[0] ?? '', series_root: '', volume: '', manage: false });
  }
  function addPlace() {
    general.locations.push({ id: 0, name: '', movie_root: '', series_root: '', volume: '', manage: false });
  }
  async function watchFound(id: number) {
    clearTimeout(foundTimer);
    const now = await api.get<any>(`/locations/${id}/found`).catch(() => null);
    if (now) foundBy[id] = now;
    if (now?.running) foundTimer = setTimeout(() => watchFound(id), 1500);
    else if (now) places = await api.get<any[]>('/locations').catch(() => places);
  }
  async function lookThrough(id: number) {
    if ((await act(() => api.post(`/locations/${id}/look`))) !== undefined) watchFound(id);
  }
  async function addFound(id: number) {
    const here = pickedOn(id);
    const chosen = here.map(([folder, m]) => ({ folder, tmdb_id: m.tmdb_id, tvmaze_id: m.tvmaze_id, tvdb_id: m.tvdb_id }));
    if ((await act(() => api.post(`/locations/${id}/add`, { picks: chosen, monitor: monitorAdopted }))) !== undefined) {
      for (const [folder] of here) delete picks[folder];
      watchFound(id);
    }
  }
  const foundIn = (found: any, state: string) => (found?.found ?? []).filter((f: any) => f.state === state);
  /** The folders on this location the user has settled. */
  const pickedOn = (id: number) => Object.entries(picks).filter(([folder]) => (foundBy[id]?.found ?? []).some((f: any) => f.folder === folder));

  async function load() {
    general = await api.get('/settings/general');
    loadPlaces();
  }
  load().catch(() => {});

  async function save() {
    for (const l of general.locations) (l.movie_profile = Number(l.movie_profile) || 0), (l.series_profile = Number(l.series_profile) || 0);
    // What is on the page is kept when it cannot be saved: it may hold a key just agreed.
    if ((await act(() => api.put('/settings/general', general), 'Saved')) === undefined) return false;
    loadStatus();
    load();
    setTimeout(loadLinks, 1500);
    return true;
  }
</script>

{#snippet mainCard()}
  {@const main = placeOf(0)}
  <div class="place">
    <div class="row wrap">
      <b>{main?.name ?? 'Library'}</b>
      {#if main}<span class="small" class:warn={!main.online}>{[main.online ? 'Reachable' : 'Cannot be reached', 'the main location', general.new_titles_location === 0 ? 'new titles go here' : '', `${main.titles} ${main.titles === 1 ? 'title' : 'titles'}`, main.bytes ? bytes(main.bytes) : '', main.free != null ? `${bytes(main.free)} free` : ''].filter(Boolean).join(' · ')}</span>{/if}
    </div>
    {#if general.movie_root || general.series_root}<div class="mono small faint where">{[general.movie_root, general.series_root].filter(Boolean).join(' · ')}</div>{/if}
    <details open={!general.movie_root && !general.series_root}>
    <summary class="small">Folders</summary>
    <div class="cols">
      <label class="field">Movies<input class="input mono" bind:value={general.movie_root} placeholder="/Volumes/Media/Movies" /></label>
      <label class="field">Series<input class="input mono" bind:value={general.series_root} placeholder="/Volumes/Media/TV Shows" /></label>
    </div>
    <span class="hint">{general.required_volume ? `These are on the disk at ${general.required_volume}. While it is not mounted, Spool pauses instead of writing somewhere else.` : 'Spool works out which disk these are on, and pauses while a disk that can be unplugged is not there.'}</span>
    <label class="check"><input type="radio" bind:group={general.new_titles_location} value={0} /> New titles go here</label>
    </details>
    {#if main}{@render checked(0)}{/if}
  </div>
{/snippet}

{#snippet placeCard(l: any, i: number)}
    {@const p = placeOf(l.id)}
    <div class="place">
      <div class="row wrap">
        <b>{l.name || 'A new location'}</b>
        {#if p}<span class="small" class:warn={!p.online}>{[p.online ? 'Reachable' : 'Cannot be reached', l.manage ? '' : 'only read', l.id && general.new_titles_location === l.id ? 'new titles go here' : '', `${p.titles} ${p.titles === 1 ? 'title' : 'titles'}`, p.bytes ? bytes(p.bytes) : '', p.free != null ? `${bytes(p.free)} free` : ''].filter(Boolean).join(' · ')}</span>{:else}<span class="faint small">Save to start using it.</span>{/if}
        <span class="grow"></span>
        {#if p}<button class="btn small" type="button" disabled={!p.online || working} onclick={() => lookThrough(l.id)}>Look for films and shows</button>{/if}
      </div>
      {#if l.movie_root || l.series_root}<div class="mono small faint where">{[l.movie_root, l.series_root].filter(Boolean).join(' · ')}</div>{/if}
      <details open={!l.id}>
      <summary class="small">Folders, and what Spool may do here</summary>
      <label class="field">Name<input class="input" bind:value={l.name} placeholder="Goliath" /></label>
      <div class="cols">
        <label class="field">Movies<input class="input mono" bind:value={l.movie_root} list={sharedBy(l)} placeholder="/Volumes/Goliath/movies" /></label>
        <label class="field">Series<input class="input mono" bind:value={l.series_root} list={sharedBy(l)} placeholder="/Volumes/Goliath/tv" /></label>
      </div>
      <label class="check"><input type="checkbox" bind:checked={l.manage} onchange={() => { if (!l.manage && general.new_titles_location === l.id) general.new_titles_location = 0; }} /> Let Spool change files here</label>
      <span class="hint">Off, Spool lists and plays what is here and changes nothing: no downloads, upgrades, renames, subtitles or deletions. On, it treats this location like the main one.</span>
      <div class="cols">
        <label class="field">Profile for films here
          <select class="input" bind:value={l.movie_profile}>
            <option value={0}>The usual one</option>
            {#each app.profiles.filter((x) => x.kind === 'movie') as x}<option value={x.id}>{x.name}</option>{/each}
          </select>
        </label>
        <label class="field">Profile for series here
          <select class="input" bind:value={l.series_profile}>
            <option value={0}>The usual one</option>
            {#each app.profiles.filter((x) => x.kind === 'tv') as x}<option value={x.id}>{x.name}</option>{/each}
          </select>
        </label>
      </div>
      <span class="hint">Given to titles added here or brought in from here; titles already in the library keep theirs. A library kept as it is wants a profile with no size target and no upgrades.</span>
      {#if l.id}<label class="check"><input type="radio" bind:group={general.new_titles_location} value={l.id} disabled={!l.manage} /> New titles go here{#if !l.manage}<span class="faint small">&nbsp;(needs "Let Spool change files here")</span>{/if}</label>{/if}
      <div><button class="btn small ghost danger" type="button" onclick={() => general.locations.splice(i, 1)}>Remove this location</button></div>
      </details>
      {#if p}{@render checked(l.id)}{/if}
      {#if foundBy[l.id]}
        {@const found = foundBy[l.id]}
        <div class="found">
          {#if found.running}
            <p class="muted">{found.step === 'adding' ? 'Adding' : 'Looking'}: {found.done} of {found.total || '…'} folders.</p>
          {:else}
            <p><b>{found.message}.</b> Nothing on the location is moved or changed by adding.</p>
            {#if foundIn(found, 'unclear').length}
              <details>
                <summary>Unclear ({foundIn(found, 'unclear').length}): choose what each is, or leave it out</summary>
                <div class="list">
                  {#each foundIn(found, 'unclear') as f (f.folder)}
                    <div class="item">
                      <div class="mono small">{f.folder}</div>
                      <div class="row wrap">
                        {#each f.matches as m}
                          <button class="btn small" class:primary={picks[f.folder] === m} type="button" onclick={() => (picks[f.folder] === m ? delete picks[f.folder] : (picks[f.folder] = m))}>{m.title}{m.year ? ` (${m.year})` : ''}</button>
                        {/each}
                      </div>
                    </div>
                  {/each}
                </div>
              </details>
            {/if}
            {#each [['unknown', 'Not recognised'], ['have', 'Already in the library'], ['failed', 'Could not be added'], ['new', 'To add'], ['added', 'Added']] as [state, label]}
              {#if foundIn(found, state).length}
                <details>
                  <summary>{label} ({foundIn(found, state).length})</summary>
                  <div class="list">
                    {#each foundIn(found, state) as f (f.folder)}
                      <div class="item"><div class="mono small">{f.folder}</div><div class="faint small">{#if f.matches[0]}{f.matches[0].title}{f.matches[0].year ? ` (${f.matches[0].year})` : ''}{#if f.note} · {/if}{/if}{f.note}</div></div>
                    {/each}
                  </div>
                </details>
              {/if}
            {/each}
            {#if foundIn(found, 'new').length + pickedOn(l.id).length}
              <div class="row wrap">
                <button class="btn primary" type="button" onclick={() => addFound(l.id)}>Add {foundIn(found, 'new').length + pickedOn(l.id).length} to the library</button>
                {#if l.manage}<label class="check"><input type="checkbox" bind:checked={monitorAdopted} /> Monitor them, so Spool looks for upgrades and missing episodes</label>{/if}
              </div>
            {/if}
          {/if}
        </div>
      {/if}
    </div>
{/snippet}

{#snippet linkCard(h: any, i: number)}
    {@const now = linkOf(h.id)}
    <div class="place">
      {#if now?.seen.problem}<p class="small warn">{now.seen.problem}</p>{/if}
      <label class="check"><input type="checkbox" bind:checked={h.play_near} /> Send players on the same network straight to it</label>
      <span class="hint">A player on the same network as {h.name || 'this machine'} takes a film that is on its disks from it directly, with a ticket for that one film, and the film no longer passes through here. A device that reaches this Spool over Tailscale counts as on the network when it is at home. Away from home it is sent to {h.name || 'the machine'} over Tailscale, if {h.name || 'the machine'} is on your Tailscale under this name; if not, and for every other player, the film comes through this Spool.</span>
      {#if now?.seen.grants?.make}
        <label class="field">Give it compact versions to make
          <select class="input" bind:value={h.make}>
            <option value="always">Whenever there are any</option>
            <option value="night">Overnight, from eleven until seven</option>
            <option value="never">Never</option>
          </select>
          <span class="hint">{now.seen.make ? `${h.name || 'It'} makes them on its video hardware. It reads each film from this machine, makes the compact version and hands it back; this machine goes on making its own meanwhile. One begun before seven in the morning is finished.` : `${h.name || 'It'} offers to make them but has ${now.seen.make_means === 'software' ? 'no video hardware, and a film is not sent across the network to be made slowly' : 'no ffmpeg installed'}, so it is given none.`}</span>
        </label>
      {/if}
      {#if !h.id && quick.has(h)}
        <label class="field">The code {h.name} gave
          <div class="row"><input class="input mono grow" inputmode="numeric" autocomplete="off" bind:value={codes[i]} placeholder="1234 5678" /><button class="btn primary" type="button" disabled={!(codes[i] ?? '').trim() || linking === i} onclick={() => linkWithCode(h, i)}>{linking === i ? 'Linking…' : 'Link'}</button></div>
          <span class="hint">It is in the helper's log, or is the one it was started with. Nothing else is needed: Spool finds its folders once the two are linked.</span>
        </label>
      {/if}
      <details open={!h.id && !quick.has(h)}>
      <summary class="small">How it is reached</summary>
      <div class="cols">
        <label class="field">Name<input class="input" bind:value={h.name} placeholder="The NAS's name" /></label>
        <label class="field">Address<input class="input mono" bind:value={h.url} placeholder="http://nas:7978" /></label>
      </div>
      <div class="cols">
        <label class="field">Code
          <div class="row"><input class="input mono grow" inputmode="numeric" autocomplete="off" bind:value={codes[i]} placeholder="1234 5678" /><button class="btn" type="button" disabled={!h.url || !(codes[i] ?? '').trim() || linking === i} onclick={() => linkWithCode(h, i)}>{linking === i ? 'Linking…' : 'Link'}</button></div>
          <span class="hint">A helper started without a key prints a code in its log. Type it here and the two agree a key between themselves.</span>
        </label>
        <label class="field">Or its key<input class="input mono" type="password" autocomplete="off" bind:value={h.key} /><span class="hint">The key the helper was started with, if it was given one.</span></label>
      </div>
      {#if now}
        <div class="field">Its folders, and where each is on this machine
          <div class="list">
            {#each now.folders ?? [] as f}<div class="small mono">{f.there} <span class="faint">is</span> {f.here}</div>{/each}
            {#each now.unfound ?? [] as there}<div class="small mono warn">{there} <span class="faint">has not been found here</span></div>{/each}
          </div>
          <span class="hint">Spool finds these by itself: {h.name || 'the machine'} marks each of its folders, and the folder here with the same mark is the same folder. A folder is found once its share is mounted on this machine. Nothing is typed, so nothing can be typed wrong.</span>
          <div><button class="btn small" type="button" onclick={() => lookAgain(h)}>Look again</button></div>
        </div>
      {/if}
      <div><button class="btn small ghost danger" type="button" onclick={() => general.links.splice(i, 1)}>Unlink {h.name || 'it'}</button></div>
      </details>
    </div>
{/snippet}

{#snippet checked(id: number)}
  <div class="row wrap">
    <button class="btn small" type="button" disabled={checks[id] === 'running'} onclick={() => checkPlace(id)}>{checks[id] === 'running' ? 'Checking…' : 'Check this location'}</button>
    {#if checks[id] && checks[id] !== 'running'}<span class="small" class:warn={!checks[id].ok}>{checks[id].ok ? 'Everything works.' : 'Something is wrong:'}</span>{/if}
  </div>
  {#if checks[id] && checks[id] !== 'running'}
    <div class="list checks">
      {#each checks[id].checks as c}
        <div class="small" class:warn={!c.ok}><b>{c.ok ? '✓' : '✗'} {c.what}</b> <span class="muted">{c.detail}</span></div>
      {/each}
    </div>
  {/if}
{/snippet}

{#if general}
  <form class="stack form" onsubmit={(e) => { e.preventDefault(); save(); }}>
    <fieldset class="card">
      <legend>Library</legend>
      <p class="muted">Where the library is: each machine that holds some of it, and under each the locations on it. A location is a folder of films and a folder of series. This machine always has one; another disk, or a share from another machine mounted here, can be added beside it. While a location cannot be reached its titles are shown as offline and left alone; nothing is searched for or downloaded again because files there cannot be seen.</p>
      <div class="machine">
        <div class="row wrap"><b>This machine</b><span class="small faint">decides everything, and serves the library</span></div>
        {@render mainCard()}
        {#each general.locations as l, i (i)}{#if !linkFor(l)}{@render placeCard(l, i)}{/if}{/each}
        <div><button class="btn" type="button" onclick={() => addPlace()}>Add a location</button></div>
      </div>
      {#each general.links ?? [] as h, hi (hi)}
        {@const now = linkOf(h.id)}
        <div class="machine">
          <div class="row wrap">
            <b>{h.name || 'A linked Spool'}</b>
            {#if now}
              <span class="small" class:warn={now.seen.grants?.work ? !now.seen.usable || !now.seen.downloads_seen : !now.seen.reachable}>
                {[!now.seen.asked ? 'Not asked yet' : !now.seen.reachable ? 'Cannot be reached' : !now.seen.usable && !now.seen.play && !now.seen.make ? 'Cannot be used' : 'Answering',
                  now.seen.reachable ? allows(now.seen) : '',
                  now.tailnet ? 'also on Tailscale' : '',
                  now.seen.usable && now.downloads ? `${now.downloads} ${now.downloads === 1 ? 'download' : 'downloads'} running` : '',
                  now.seen.free != null ? `${bytes(now.seen.free)} free for downloads` : ''].filter(Boolean).join(' · ')}
              </span>
            {:else}
              <span class="faint small">Save to start using it.</span>
            {/if}
          </div>
          {@render linkCard(h, hi)}
          {#each general.locations as l, i (i)}{#if linkFor(l) === h}{@render placeCard(l, i)}{/if}{/each}
          <datalist id={`shared-${hi}`}>{#each sharedHere(h) as folder}<option value={folder}></option>{/each}</datalist>
          <div><button class="btn" type="button" disabled={!(h.folders ?? []).length} onclick={() => addPlaceOn(h)}>Add a location on {h.name || 'it'}</button></div>
        </div>
      {/each}
      {#if notAdded.length}
        <div class="machine">
          <div class="row wrap"><b>Found on this network</b><span class="small faint">not linked; each says it is a Spool, and its code or key is what shows that it is yours</span></div>
          {#each notAdded as f (f.url)}
            <div class="place">
              <div class="row wrap">
                <b>{f.name}</b>
                <span class="small mono faint">{f.url}</span>
                <span class="small">{f.open ? 'waiting to be linked with a code' : 'linked to a Spool already'}</span>
                <span class="grow"></span>
                <button class="btn small" type="button" onclick={() => addLink(f)}>Link it</button>
              </div>
            </div>
          {/each}
          <div></div>
        </div>
      {/if}
      <div><button class="btn" type="button" onclick={() => addLink()}>Link another Spool</button></div>
      <p class="hint">A linked Spool is, today, a helper: a small Spool on a machine that holds some of the disks, such as a NAS. It downloads, unpacks and files onto its own disks, moves titles between them, looks through its own folders and can hand a film to a player itself, so that nothing crosses the network to this machine and back. Without one, or while one cannot be reached, a location on another machine works across the share. See the <a href="https://rewtraw.github.io/spool/guide/helper.html" target="_blank" rel="noreferrer">guide</a>.</p>
      <p class="hint">Titles already in the library stay where they are. When the place chosen for new titles cannot be reached, they go to this machine's main location. A title being added can be put somewhere else from its own box.</p>
    </fieldset>

    <div><button class="btn primary" type="submit">Save</button></div>
  </form>
{/if}
