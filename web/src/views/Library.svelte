<script lang="ts">
  // Everything Spool keeps, in one place: films and series, the albums in the music folders
  // and those wanted, and books. "Everything" is all of them together; each kind can be
  // looked at on its own, and films and series further as movies, series or anime.
  import { app, go, act, toast, loadTitles, route } from '../lib/state.svelte';
  import { api } from '../lib/api';
  import type { Book, Collection, Title } from '../lib/api';
  import { bytes } from '../lib/format';
  import Tile from '../components/Tile.svelte';

  type Scope = 'all' | 'video' | 'music' | 'books';
  type VideoKind = 'all' | 'movie' | 'series' | 'anime';
  const saved = (() => {
    try {
      return JSON.parse(localStorage.getItem('spool.library') ?? '{}');
    } catch {
      return {};
    }
  })();
  const askedScope = route.query.get('kind');
  let scope = $state<Scope>((['all', 'video', 'music', 'books'].includes(askedScope ?? '') ? askedScope : saved.scope ?? 'all') as Scope);
  let kind = $state<VideoKind>(saved.kind ?? 'all');
  let show = $state<string>(saved.show ?? 'all');
  let sort = $state<string>(saved.sort ?? 'title');
  let layout = $state<'grid' | 'table'>(saved.layout ?? 'grid');
  /** Show each collection as one tile in place of its titles. */
  let grouped = $state<boolean>(saved.grouped ?? false);
  /** Only what is kept on this location, as its home or as a copy; empty for everywhere. */
  let place = $state<string>(saved.place ?? '');
  const placeNames = $derived([...new Set(app.titles.flatMap((t) => [t.location, ...(t.also_on ?? [])]).filter(Boolean) as string[])].sort());
  /** Anime, films and series alike: what AniList has an entry for. */
  const isAnime = (t: Title) => !!t.anilist?.length;
  const onPlace = (t: Title, name: string) => t.location === name || (t.also_on ?? []).includes(name);
  let query = $state('');
  let limit = $state(120);

  $effect(() => {
    try {
      localStorage.setItem('spool.library', JSON.stringify({ scope, kind, show, sort, layout, grouped, place }));
    } catch {}
  });

  // ---- music and books, beside the titles every page already has
  let albums = $state<any[]>([]);
  let wants = $state<any[]>([]);
  let books = $state<Book[]>([]);
  let othersLoaded = $state(false);
  Promise.all([
    api.get<{ albums: any[] }>('/music').then((r) => (albums = r.albums)).catch(() => {}),
    app.manages ? api.get<{ wants: any[] }>('/music/wants').then((r) => (wants = r.wants.map((w) => ({ ...w.want, cover: w.cover })))).catch(() => {}) : null,
    app.manages ? api.get<{ books: Book[] }>('/books').then((r) => (books = r.books)).catch(() => {}) : null,
  ]).then(() => (othersLoaded = true));

  /** One thing in the library, whatever it is. */
  type Item = {
    key: string;
    type: 'title' | 'album' | 'want' | 'book';
    /** What it is, in a word: Film, Series, Album, Book. */
    label: string;
    name: string;
    sortName: string;
    /** Who made it: an album's artist, a book's author. */
    by: string;
    year: number;
    added: number;
    size: number;
    href: string;
    cover?: string | null;
    square: boolean;
    /** Here to play or read, rather than wanted. */
    have: boolean;
    status: { label: string; tone: string };
    t?: Title;
    lossless?: boolean;
    state?: string;
  };

  /** A name as it is sorted: without quotes or other marks before it, or "The" and "A". */
  const sortable = (s: string) => s.toLowerCase().replace(/^[^\p{L}\p{N}]+/u, '').replace(/^(the|a|an) /, '');

  function have(t: Title) {
    return t.kind === 'movie' ? t.file_count > 0 : t.episodes_aired > 0 && t.episodes_have >= t.episodes_aired;
  }

  function statusOf(t: Title): { label: string; tone: string } {
    if (t.offline) return { label: 'Offline', tone: '' };
    if (t.active === 'import_blocked') return { label: 'Needs attention', tone: 'warn' };
    if (t.active) return { label: 'Downloading', tone: 'accent' };
    if (t.kind === 'series') {
      if (t.episodes_aired === 0) return { label: t.episodes_have ? `${t.episodes_have} eps` : 'Not aired', tone: '' };
      if (t.episodes_have >= t.episodes_aired) return { label: `${t.episodes_have} / ${t.episodes_aired}`, tone: 'ok' };
      return { label: `${t.episodes_have} / ${t.episodes_aired}`, tone: t.monitored ? 'warn' : '' };
    }
    if (t.file_count > 0) return { label: 'In library', tone: 'ok' };
    if (!t.monitored) return { label: 'Not monitored', tone: '' };
    if (!t.available) return { label: 'Not released', tone: '' };
    return { label: 'Missing', tone: 'warn' };
  }

  const LOSSLESS = ['flac', 'alac', 'wav', 'aiff', 'aif', 'wv', 'ape'];
  const BOOK_STATE: Record<string, { label: string; tone: string }> = {
    wanted: { label: 'Wanted', tone: 'warn' },
    choose: { label: 'Choose a file', tone: 'accent' },
    fetching: { label: 'Downloading', tone: 'accent' },
    sending: { label: 'Sending', tone: 'accent' },
    have: { label: 'Got it', tone: 'ok' },
  };

  const items = $derived.by((): Item[] => {
    const out: Item[] = app.titles.map((t) => ({
      key: `t${t.id}`, type: 'title', label: t.kind === 'movie' ? 'Film' : 'Series', name: t.title, sortName: t.sort_title, by: '', year: t.year, added: t.added_at, size: t.size,
      href: `#/title/${t.id}`, cover: t.poster_small ?? t.poster, square: false, have: t.kind === 'movie' ? t.file_count > 0 : t.episodes_have > 0, status: statusOf(t), t,
    }));
    for (const a of albums) {
      const lossless = LOSSLESS.includes(a.format) || (a.format === 'm4a' && a.bit_depth > 0);
      out.push({
        key: `a${a.id}`, type: 'album', label: 'Album', name: a.title, sortName: sortable(a.title), by: a.artist, year: a.year, added: a.added_at, size: a.bytes,
        href: `#/music/${a.id}`, cover: a.has_cover ? `/api/music/albums/${a.id}/art?size=300` : null, square: true, have: true,
        status: { label: (a.format ?? '').toUpperCase(), tone: '' }, lossless,
      });
    }
    // An album wanted and since filed is shown once, as the album.
    for (const w of wants.filter((w) => w.state !== 'have')) {
      out.push({
        key: `w${w.id}`, type: 'want', label: 'Album', name: w.title, sortName: sortable(w.title), by: w.artist, year: w.year, added: w.added_at, size: w.release?.size ?? 0,
        href: '#/music/wanted', cover: w.cover, square: true, have: false, status: w.state === 'downloading' ? { label: 'Downloading', tone: 'accent' } : { label: 'Wanted', tone: 'warn' }, state: w.state,
      });
    }
    for (const b of books) {
      out.push({
        key: `b${b.id}`, type: 'book', label: 'Book', name: b.title, sortName: sortable(b.title), by: b.authors.join(', '), year: b.year, added: b.added_at, size: b.chosen?.size ?? 0,
        href: `#/books/${b.id}`, cover: b.cover, square: false, have: b.state === 'have', status: BOOK_STATE[b.state] ?? { label: b.state, tone: '' }, state: b.state,
      });
    }
    return out;
  });

  const inScope = (i: Item, s: Scope) => s === 'all' || (s === 'video' ? i.type === 'title' : s === 'music' ? i.type === 'album' || i.type === 'want' : i.type === 'book');

  // What can be shown and sorted by depends on what is being looked at.
  const SHOWS: Record<Scope, [string, string][]> = $derived({
    all: [['all', 'Everything'], ['have', 'Here now'], ['wanted', 'Wanted, not here yet']],
    video: [
      ['all', 'Everything'], ['have', 'Have files'], ['missing', 'Missing (wanted)'], ['incomplete', 'Incomplete series'], ['compact', 'Have a compact version'], ['nocompact', 'Have files, no compact version'],
      ...((app.me ? [['unwatched', 'Not watched'], ['watching', 'Part-way through'], ['watched', 'Watched']] : []) as [string, string][]),
      ...((app.manages ? [['monitored', 'Monitored'], ['unmonitored', 'Not monitored']] : []) as [string, string][]),
    ],
    music: [['all', 'Everything'], ['have', 'In the music folder'], ['wanted', 'Wanted, not here yet'], ['lossless', 'Lossless'], ['lossy', 'Lossy']],
    books: [['all', 'Everything'], ['have', 'Got'], ['wanted', 'Wanted, not here yet'], ['choose', 'Waiting for a choice']],
  });
  const SORTS = $derived<[string, string][]>([
    ['title', 'Title'],
    ...((scope === 'music' ? [['by', 'Artist']] : scope === 'books' ? [['by', 'Author']] : []) as [string, string][]),
    ['added', 'Recently added'],
    ['year', 'Year'],
    ['size', 'Size'],
  ]);
  // A choice the new view does not offer falls back to the first it does.
  $effect(() => {
    if (!SHOWS[scope].some(([k]) => k === show)) show = 'all';
    if (!SORTS.some(([k]) => k === sort)) sort = 'title';
  });

  function passes(i: Item): boolean {
    const t = i.t;
    if (t) {
      if (scope === 'video' && (kind === 'anime' ? !isAnime(t) : kind !== 'all' && t.kind !== kind)) return false;
      if (place && placeNames.includes(place) && !onPlace(t, place)) return false;
      // Someone who only watches is shown what there is to watch.
      if (!app.manages && !have(t)) return false;
    } else if (place && placeNames.includes(place)) return false;
    switch (show) {
      case 'have': return i.have;
      case 'wanted': return !i.have && (!t || (t.monitored && t.available));
      case 'lossless': return i.type === 'album' && !!i.lossless;
      case 'lossy': return i.type === 'album' && !i.lossless;
      case 'choose': return i.state === 'choose';
    }
    if (!t) return show === 'all';
    if (show === 'missing' && (have(t) || !t.monitored || !t.available)) return false;
    // A series with some of its aired episodes and not all, whether or not it is monitored.
    if (show === 'incomplete' && !(t.kind === 'series' && t.episodes_have > 0 && t.episodes_have < t.episodes_aired)) return false;
    if (show === 'compact' && !t.compact_count) return false;
    if (show === 'nocompact' && (t.compact_count || !have(t))) return false;
    if (show === 'unwatched' && (!have(t) || t.watched?.done)) return false;
    if (show === 'watching' && !(t.watched && !t.watched.done && t.watched.part)) return false;
    if (show === 'watched' && !t.watched?.done) return false;
    if (show === 'monitored' && !t.monitored) return false;
    if (show === 'unmonitored' && t.monitored) return false;
    return true;
  }

  const filtered = $derived.by(() => {
    const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
    const list = items.filter((i) => {
      if (!inScope(i, scope) || !passes(i)) return false;
      const text = `${i.name} ${i.by} ${i.year || ''}`.toLowerCase();
      return words.every((w) => text.includes(w));
    });
    const byName = (a: Item, b: Item) => a.sortName.localeCompare(b.sortName);
    const by: Record<string, (a: Item, b: Item) => number> = {
      title: byName,
      by: (a, b) => sortable(a.by).localeCompare(sortable(b.by)) || a.year - b.year || byName(a, b),
      added: (a, b) => b.added - a.added,
      year: (a, b) => b.year - a.year || byName(a, b),
      size: (a, b) => b.size - a.size,
    };
    return list.sort(by[sort] ?? byName);
  });
  /** The films and series among what is shown: what selecting acts on. */
  const filteredTitles = $derived(filtered.flatMap((i) => (i.t ? [i.t] : [])));
  // With collections gathered, a collection stands where its first title would, and holds
  // those of its titles the filters let through. One title alone is shown as itself.
  type Entry = { i: Item; c?: undefined } | { c: Collection; inside: Title[]; i?: undefined };
  const entries = $derived.by((): Entry[] => {
    if (!grouped || selecting) return filtered.map((i) => ({ i }));
    const of = new Map<number, Collection>();
    for (const c of app.collections) for (const m of c.members) of.set(m.title_id, c);
    const inside = new Map<number, Title[]>();
    for (const t of filteredTitles) {
      const c = of.get(t.id);
      if (c) inside.set(c.id, [...(inside.get(c.id) ?? []), t]);
    }
    const out: Entry[] = [];
    const placed = new Set<number>();
    for (const i of filtered) {
      const c = i.t && of.get(i.t.id);
      if (!c || inside.get(c.id)!.length < 2) out.push({ i });
      else if (!placed.has(c.id)) {
        placed.add(c.id);
        out.push({ c, inside: inside.get(c.id)! });
      }
    }
    return out;
  });
  const shown = $derived(entries.slice(0, limit));

  $effect(() => {
    // Reset paging when the filter changes.
    scope; kind; show; sort; query; place;
    limit = 120;
  });

  let sentinel = $state<HTMLElement>();
  $effect(() => {
    if (!sentinel) return;
    const io = new IntersectionObserver((entries) => {
      if (entries.some((e) => e.isIntersecting)) limit += 120;
    }, { rootMargin: '600px' });
    io.observe(sentinel);
    return () => io.disconnect();
  });

  // ---- acting on several titles at once
  let selecting = $state(false);
  let picked = $state<Record<number, boolean>>({});
  let lastPicked = -1;
  const chosen = $derived(app.titles.filter((t) => picked[t.id]));
  const chosenBytes = $derived(chosen.reduce((s, t) => s + (t.size || 0), 0));
  let bulkProfile = $state('');
  // Moving a selection to another location.
  let places = $state<any[]>([]);
  api.get<any[]>('/locations').then((p) => (places = p)).catch(() => {});
  const targets = $derived(places.filter((p) => p.manage && p.online));
  async function bulkCompact() {
    const r = await act(() => api.post<any>('/compact', { title_ids: chosen.map((t) => t.id) }));
    if (!r) return;
    const first = r.refused?.[0];
    toast(r.queued ? `${r.queued} compact ${r.queued === 1 ? 'version is' : 'versions are'} in line. Progress is in Activity.${first ? ` ${r.refused.length} left out: ${first.why}.` : ''}` : `None to make${first ? `: ${first.why}` : ': nothing selected would gain from one'}.`);
    picked = {};
  }
  async function bulkMove(where: string) {
    const place = places.find((p) => String(p.id) === where);
    if (!place) return;
    const n = chosen.length;
    if (!confirm(`Move ${n} ${n === 1 ? 'title' : 'titles'} (${bytes(chosenBytes)}) to ${place.name}? They are moved one at a time; each original is removed once its copy has been checked, or kept as a copy where Spool only reads.`)) return;
    const r = await act(() => api.post<any>('/titles/move', { ids: chosen.map((t) => t.id), location: place.id }));
    if (r) {
      // Say why for the first that was refused; the rest are usually for the same reason.
      const first = r.refused?.[0];
      toast(!first ? `Moving ${r.queued} to ${place.name}` : `Moving ${r.queued} of ${r.asked} to ${place.name}. Not moved: ${first.why}${r.refused.length > 1 ? `, and ${r.refused.length - 1} more` : ''}.`);
    }
    picked = {};
  }

  function stopSelecting() {
    selecting = false;
    picked = {};
    lastPicked = -1;
  }
  // A click ticks one; a shift-click ticks everything between it and the last one ticked.
  function pick(e: MouseEvent | KeyboardEvent, t: Title) {
    e.preventDefault();
    const at = filteredTitles.findIndex((x) => x.id === t.id);
    const on = !picked[t.id];
    if (e.shiftKey && lastPicked >= 0 && at >= 0) {
      const [a, b] = at < lastPicked ? [at, lastPicked] : [lastPicked, at];
      for (const x of filteredTitles.slice(a, b + 1)) picked[x.id] = on;
    } else {
      picked[t.id] = on;
    }
    lastPicked = at;
  }
  function pickAll() {
    const all = filteredTitles.every((t) => picked[t.id]);
    for (const t of filteredTitles) picked[t.id] = !all;
  }
  const many = (n: number) => `${n} ${n === 1 ? 'title' : 'titles'}`;
  async function bulk(action: string, extra: Record<string, unknown> = {}, done?: (r: any) => string) {
    const ids = chosen.map((t) => t.id);
    if (!ids.length) return;
    const r = await act(() => api.post<any>('/titles/bulk', { ids, action, ...extra }));
    if (!r) return;
    toast(done ? done(r) : `Done for ${many(r.done ?? ids.length)}`);
    await loadTitles();
    if (action === 'remove' || action === 'free') stopSelecting();
  }
  function bulkFree() {
    const withFiles = chosen.filter((t) => t.size > 0).length;
    if (!withFiles) return toast('None of these have files on disk');
    if (!confirm(`Delete the files of ${many(withFiles)} (${bytes(chosenBytes)})? They stay in the library, unmonitored, so Spool does not fetch them again.`)) return;
    bulk('free', {}, (r) => `Deleted the files of ${many(r.done)}, ${bytes(r.bytes)}`);
  }
  function bulkRemove(deleteFiles: boolean) {
    const n = chosen.length;
    if (!confirm(deleteFiles ? `Remove ${many(n)} from the library and delete their files (${bytes(chosenBytes)})?` : `Remove ${many(n)} from the library? Their files stay on disk.`)) return;
    bulk('remove', { delete_files: deleteFiles }, (r) => `Removed ${many(r.done)}`);
  }
  function bulkSetProfile() {
    if (!bulkProfile) return;
    bulk('profile', { profile_id: Number(bulkProfile) }, (r) => (r.done ? `Profile changed for ${many(r.done)}` : 'That profile does not apply to any of the selected titles'));
    bulkProfile = '';
  }

  // The counts follow the location chosen, so they say what the tabs will show.
  const inPlace = $derived(place && placeNames.includes(place) ? app.titles.filter((t) => onPlace(t, place)) : app.titles);
  const seen = $derived(app.manages ? inPlace : inPlace.filter(have));
  const counts = $derived({
    video: seen.length,
    movie: seen.filter((t) => t.kind === 'movie').length,
    series: seen.filter((t) => t.kind === 'series').length,
    anime: seen.filter(isAnime).length,
    music: place ? 0 : albums.length + wants.filter((w) => w.state !== 'have').length,
    books: place ? 0 : books.length,
  });
  const scopes = $derived(
    ([['all', 'Everything', counts.video + counts.music + counts.books], ['video', 'Film & TV', counts.video], ['music', 'Music', counts.music], ['books', 'Books', counts.books]] as [Scope, string, number][]).filter(
      ([k]) => k !== 'books' || app.manages,
    ),
  );
  const noun = $derived(scope === 'video' ? ['title', 'titles'] : scope === 'music' ? ['album', 'albums'] : scope === 'books' ? ['book', 'books'] : ['item', 'items']);
  const discoverKind: Record<Scope, string> = { all: 'all', video: 'all', music: 'music', books: 'books' };
</script>

<header class="head">
  <h1>Library</h1>
  {#if app.manages}
    <div class="row">
      {#if scope === 'all' || scope === 'video'}<button class="btn" onclick={() => (selecting ? stopSelecting() : (selecting = true))} aria-pressed={selecting}>{selecting ? 'Done' : 'Select'}</button>{/if}
      <button class="btn primary" onclick={() => go(`discover?kind=${discoverKind[scope]}`)}>Add</button>
    </div>
  {/if}
</header>

<div class="bar">
  <div class="seg" role="tablist" aria-label="What">
    {#each scopes as [k, label, n]}
      <button role="tab" aria-selected={scope === k} class:on={scope === k} onclick={() => { scope = k; if (k !== 'all' && k !== 'video') stopSelecting(); }}>{label} <small>{n}</small></button>
    {/each}
  </div>
  {#if scope === 'video'}
    <div class="seg sub" role="tablist" aria-label="Film and TV">
      {#each [['all', 'All'], ['movie', 'Movies'], ['series', 'Series'], ['anime', 'Anime']] as [k, label]}
        <button role="tab" aria-selected={kind === k} class:on={kind === k} onclick={() => (kind = k as VideoKind)}>{label} <small>{k === 'all' ? counts.video : counts[k as 'movie']}</small></button>
      {/each}
    </div>
  {/if}
</div>

<div class="bar">
  <input class="input search" type="search" placeholder={scope === 'music' ? 'Filter by album, artist or year' : scope === 'books' ? 'Filter by title, author or year' : scope === 'video' ? 'Filter by title or year' : 'Filter by name, artist, author or year'} bind:value={query} data-search aria-label="Filter library" />
  <select class="input narrow" bind:value={show} aria-label="Show">
    {#each SHOWS[scope] as [k, label]}<option value={k}>{label}</option>{/each}
  </select>
  {#if placeNames.length > 1 && (scope === 'all' || scope === 'video')}
    <select class="input narrow" bind:value={place} aria-label="Location">
      <option value="">Every location</option>
      {#each placeNames as name}<option value={name}>{name}</option>{/each}
    </select>
  {/if}
  <select class="input narrow" bind:value={sort} aria-label="Sort by">
    {#each SORTS as [k, label]}<option value={k}>{label}</option>{/each}
  </select>
  {#if scope === 'all' || scope === 'video'}
    <button class="btn toggle" class:on={grouped} aria-pressed={grouped} onclick={() => (grouped = !grouped)} title="Show each collection as one tile">Collections</button>
  {/if}
  <div class="seg" aria-label="Layout">
    <button class:on={layout === 'grid'} onclick={() => (layout = 'grid')} aria-pressed={layout === 'grid'}>Grid</button>
    <button class:on={layout === 'table'} onclick={() => (layout = 'table')} aria-pressed={layout === 'table'}>Table</button>
  </div>
</div>

{#if !app.titlesLoaded || (scope !== 'video' && !othersLoaded)}
  <div class="empty">Loading…</div>
{:else if !filtered.length}
  <div class="card empty">
    {#if items.some((i) => inScope(i, scope))}Nothing matches these filters.
    {:else if scope === 'music'}No music yet. Add a music folder to a location in <a class="link" href="#/settings/library">Settings → Library</a>, or <a class="link" href="#/discover?kind=music">find an album</a> to want.
    {:else if scope === 'books'}No books yet. <a class="link" href="#/discover?kind=books">Find one</a> to add.
    {:else}The library is empty. Add a title to get started.{/if}
  </div>
{:else if layout === 'grid'}
  <div class="grid">
    {#each shown as e (e.c ? `c${e.c.id}` : e.i.key)}
      {#if e.c}
        <Tile href="#/collection/{e.c.id}" src={e.c.poster ?? e.inside[0].poster_small ?? e.inside[0].poster} title={e.c.name} sub="Collection" stack chip={e.c.missing.length ? `${e.c.members.length} of ${e.c.members.length + e.c.missing.length}` : `${e.inside.length} titles`} />
      {:else if e.i.t}
        {@const t = e.i.t}
        {@const s = e.i.status}
        <Tile href="#/title/{t.id}" src={t.poster_small ?? t.poster} title={t.title} kind={t.kind} sub={`${t.year || ''}${t.kind === 'series' ? ' · Series' : ''}`} chip={app.manages || t.offline ? s.label : ''} tone={s.tone} watched={selecting ? undefined : t.watched} dim={t.offline || (!t.monitored && !(t.file_count || t.episodes_have))} picked={picked[t.id]} onclick={(e) => selecting && pick(e, t)}>
          {#if selecting}<span class="check" class:on={picked[t.id]} aria-hidden="true">{picked[t.id] ? '✓' : ''}</span>{/if}
        </Tile>
      {:else}
        {@const i = e.i}
        <Tile href={i.href} src={i.cover} title={i.name} kind={i.type === 'book' ? 'book' : 'album'} square={i.square} sub={[i.by, i.year || ''].filter(Boolean).join(' · ')} chip={!i.have || i.type === 'book' ? i.status.label : ''} tone={i.status.tone} dim={selecting || !i.have} onclick={(ev) => selecting && ev.preventDefault()} />
      {/if}
    {/each}
  </div>
{:else}
  <div class="card scroll-x">
    <table class="table">
      <thead><tr>{#if selecting}<th style="width:1%"><input class="tick" type="checkbox" aria-label="Select all" checked={filteredTitles.length > 0 && filteredTitles.every((t) => picked[t.id])} onchange={pickAll} /></th>{/if}<th>Title</th><th>Year</th><th>Type</th><th>Status</th>{#if placeNames.length > 1 && scope !== 'music' && scope !== 'books'}<th>Location</th>{/if}{#if scope !== 'music' && scope !== 'books'}<th>Profile</th>{/if}<th>Size</th></tr></thead>
      <tbody>
        {#each shown as e (e.c ? `c${e.c.id}` : e.i.key)}
          {#if e.c}
          <tr onclick={() => go(`collection/${e.c.id}`)}>
            {#if selecting}<td></td>{/if}
            <td><a href="#/collection/{e.c.id}"><b>{e.c.name}</b></a></td>
            <td class="muted">{Math.min(...e.inside.map((t) => t.year).filter(Boolean)) || ''}</td>
            <td class="muted">Collection</td>
            <td><span class="chip">{e.inside.length} titles</span></td>
            {#if placeNames.length > 1 && scope !== 'music' && scope !== 'books'}<td></td>{/if}
            {#if scope !== 'music' && scope !== 'books'}<td></td>{/if}
            <td class="muted" style="white-space:nowrap">{bytes(e.inside.reduce((n, t) => n + t.size, 0))}</td>
          </tr>
          {:else}
          {@const i = e.i}
          {@const t = i.t}
          <tr onclick={(ev) => (selecting ? t && pick(ev, t) : (location.hash = i.href))} class:picked={t && picked[t.id]} class:dim={selecting && !t}>
            {#if selecting}<td>{#if t}<input class="tick" type="checkbox" aria-label="Select {t.title}" checked={!!picked[t.id]} />{/if}</td>{/if}
            <td><a href={i.href}><b>{i.name}</b></a>{#if i.by}<div class="muted small">{i.by}</div>{/if}</td>
            <td class="muted">{i.year || ''}</td>
            <td class="muted">{i.label}</td>
            <td><span class="chip {i.status.tone}">{i.status.label || (i.have ? 'Here' : '')}</span>{#if t && !t.monitored}<span class="faint small"> not monitored</span>{/if}</td>
            {#if placeNames.length > 1 && scope !== 'music' && scope !== 'books'}<td class="muted">{t?.location ?? ''}{#if t?.also_on?.length}<span class="faint small"> +{t.also_on.join(', ')}</span>{/if}</td>{/if}
            {#if scope !== 'music' && scope !== 'books'}<td class="muted">{t?.profile ?? ''}</td>{/if}
            <td class="muted" style="white-space:nowrap">{i.size ? bytes(i.size) : ''}</td>
          </tr>
          {/if}
        {/each}
      </tbody>
    </table>
  </div>
{/if}
<div bind:this={sentinel}></div>
{#if filtered.length}
  <p class="faint count">{filtered.length} {filtered.length === 1 ? noun[0] : noun[1]}</p>
{/if}
{#if selecting}
  <div class="bulkbar" role="toolbar" aria-label="Actions for selected titles">
    <span class="n">{chosen.length} selected{chosenBytes ? ` · ${bytes(chosenBytes)}` : ''}</span>
    <button class="btn small ghost" onclick={pickAll}>{filteredTitles.every((t) => picked[t.id]) ? 'Select none' : `Select all ${filteredTitles.length}`}</button>
    <span class="sep"></span>
    {#if chosen.length}
      <button class="btn small" onclick={() => bulk('monitor', {}, (r) => `Monitoring ${many(r.done)}`)}>Monitor</button>
      <button class="btn small" onclick={() => bulk('unmonitor', {}, (r) => `Stopped monitoring ${many(r.done)}`)}>Stop monitoring</button>
      <select class="input small-select" bind:value={bulkProfile} onchange={bulkSetProfile} aria-label="Change profile">
        <option value="">Change profile…</option>
        {#each app.profiles as p}<option value={p.id}>{p.name} ({p.kind === 'movie' ? 'movies' : 'series'})</option>{/each}
      </select>
      <button class="btn small" onclick={() => bulk('search', {}, (r) => `Searching ${many(r.started)} in the background. Downloads appear in Activity.`)}>Search</button>
      <button class="btn small" onclick={() => bulk('refresh', {}, (r) => `Refreshing ${many(r.started)} in the background`)}>Refresh</button>
      <button class="btn small" title="Make a smaller copy of each, kept beside the original, for playing away from home" onclick={bulkCompact}>Make compact versions</button>
      {#if targets.length > 1}
        <select class="input small-select" value={''} onchange={(e) => { bulkMove(e.currentTarget.value); e.currentTarget.value = ''; }} aria-label="Move to a location">
          <option value="">Move to…</option>
          {#each targets as p}<option value={p.id}>{p.name}{p.free != null ? ` (${bytes(p.free)} free)` : ''}</option>{/each}
        </select>
      {/if}
      <button class="btn small ghost danger" onclick={bulkFree}>Delete files</button>
      <button class="btn small ghost danger" onclick={() => bulkRemove(false)}>Remove</button>
      <button class="btn small ghost danger" onclick={() => bulkRemove(true)}>Remove and delete files</button>
    {:else}
      <span class="faint">Click titles to select them. Shift-click selects a run.</span>
    {/if}
  </div>
{/if}

<style>
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 18px;
  }
  .bar {
    display: flex;
    gap: 10px;
    flex-wrap: wrap;
    align-items: center;
    margin-bottom: 20px;
  }
  .search {
    flex: 1 1 220px;
    width: auto;
  }
  .narrow {
    width: auto;
    flex: 0 1 auto;
  }
  .seg {
    display: inline-flex;
    padding: 3px;
    border-radius: 9px;
    background: var(--raised);
    border: 1px solid var(--line);
  }
  .seg button {
    height: 28px;
    padding: 0 11px;
    border: none;
    background: transparent;
    border-radius: 6px;
    cursor: pointer;
    font-size: 13.5px;
    font-weight: 500;
    color: var(--muted);
    white-space: nowrap;
  }
  .seg button.on {
    background: var(--surface);
    color: var(--text);
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.18);
  }
  .seg {
    max-width: 100%;
    overflow-x: auto;
  }
  .seg.sub button {
    height: 24px;
    font-size: 12.5px;
  }
  tr.dim {
    opacity: 0.5;
  }
  .seg small {
    color: var(--faint);
    font-weight: 400;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(138px, 1fr));
    gap: 22px 16px;
    /* Album covers are square and posters tall: each sits at the top of its row. */
    align-items: start;
  }
  .toggle.on {
    background: var(--accent-soft);
    border-color: var(--accent);
    color: var(--text);
  }
  .check {
    position: absolute;
    top: 7px;
    right: 7px;
    width: 24px;
    height: 24px;
    display: grid;
    place-items: center;
    border-radius: 50%;
    border: 2px solid #fff;
    background: rgba(0, 0, 0, 0.45);
    color: #fff;
    font-size: 14px;
    font-weight: 700;
  }
  .check.on {
    background: var(--accent);
    border-color: var(--accent);
  }
  tr.picked {
    background: var(--accent-soft);
  }
  .small-select {
    width: auto;
    height: 30px;
    padding: 0 8px;
    font-size: 13.5px;
  }
  .small {
    font-size: 12.5px;
  }
  tbody tr {
    cursor: pointer;
  }
  @media (hover: hover) {
    tbody tr:hover {
      background: var(--raised);
    }
  }
  .count {
    margin-top: 18px;
    font-size: 13px;
  }
  @media (max-width: 760px) {
    .grid {
      grid-template-columns: repeat(auto-fill, minmax(104px, 1fr));
      gap: 16px 10px;
    }
  }
</style>
