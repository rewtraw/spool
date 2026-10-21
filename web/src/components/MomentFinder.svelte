<script lang="ts">
  // Find moments across the library by what is said in them, and make clips of those chosen.
  import { api, type Coverage, type Found, type Moment } from '../lib/api';
  import { toast } from '../lib/state.svelte';
  import { clock } from '../lib/format';

  let { onmade }: { onmade: () => void } = $props();

  let query = $state('');
  let found = $state<Found | null>(null);
  let coverage = $state<Coverage | null>(null);
  let looking = $state(false);
  let error = $state('');
  let picked = $state<Record<string, boolean>>({});
  let lead = $state(2);
  let tail = $state(1.5);
  let subtitles = $state('auto');

  const key = (m: Moment) => `${m.file_id}:${m.start}`;
  const chosen = $derived(found?.moments.filter((m) => picked[key(m)]) ?? []);

  async function loadCoverage() {
    coverage = await api.get('/dialogue/coverage').catch(() => coverage);
  }
  $effect(() => {
    loadCoverage();
    const timer = setInterval(loadCoverage, 15000);
    return () => clearInterval(timer);
  });

  async function look(e: Event) {
    e.preventDefault();
    if (!query.trim()) return;
    looking = true;
    error = '';
    try {
      found = await api.get(`/dialogue?q=${encodeURIComponent(query.trim())}&limit=60`);
      coverage = found!.coverage;
      picked = {};
    } catch (err: any) {
      error = err.message;
    } finally {
      looking = false;
    }
  }

  /** What a clip of a moment is called: where it is from, and the first words said. */
  function nameOf(m: Moment) {
    const words = m.lines[0]?.text.replace(/\s+/g, ' ') ?? '';
    const short = words.length > 48 ? `${words.slice(0, 46).trimEnd()}…` : words;
    return `${m.from.split(' · ')[0]} · “${short}”`;
  }

  async function make() {
    const list = chosen;
    let made = 0;
    const failed: string[] = [];
    for (const m of list) {
      const start = Math.max(0, m.start - lead);
      const seconds = Math.max(3, m.end + tail - start);
      try {
        await api.post('/clips', { title_id: m.title_id, episode_id: m.episode_id ?? undefined, start: Math.round(start * 10) / 10, seconds: Math.round(seconds * 10) / 10, name: nameOf(m), subtitles });
        made++;
      } catch (err: any) {
        failed.push(`${m.from}: ${err.message}`);
      }
    }
    if (made) toast(`Making ${made === 1 ? 'one clip' : `${made} clips`}. They appear below as each is ready.`);
    if (failed.length) toast(`Not made: ${failed.join('; ')}`, 'error');
    picked = {};
    onmade();
  }

  const share = (c: Coverage) => (c.files ? Math.round((c.read / c.files) * 100) : 0);
</script>

<section class="card finder">
  <form class="row look" onsubmit={look}>
    <input class="input grow" bind:value={query} placeholder="What is said: wake up; don't go to sleep" aria-label="What is said" autocomplete="off" spellcheck="false" />
    <button class="btn primary" type="submit" disabled={looking || !query.trim()}>{looking ? 'Looking…' : 'Find'}</button>
  </form>
  {#if coverage}
    <p class="faint small about">
      {#if coverage.files}
        Searches what is said in {coverage.read.toLocaleString()} of {coverage.files.toLocaleString()} films and episodes ({share(coverage)}%), by the words and by what they mean.
        {#if coverage.unreadable - coverage.not_found > 0}
          {#if coverage.can_fetch}
            {coverage.unreadable - coverage.not_found} more once their subtitles are fetched.
          {:else}
            {coverage.unreadable} have no subtitles to read: with an OpenSubtitles account set in Settings, they are fetched for them.
          {/if}
        {/if}
        {#if coverage.passages > coverage.placed}Still working out what {(coverage.passages - coverage.placed).toLocaleString()} passages mean.{/if}
      {:else}
        The library is being read for what is said in it.
      {/if}
      {#if coverage.doing}<span class="doing">{coverage.doing}…</span>{/if}
    </p>
    {#each coverage.problems as p}<p class="small warn">{p}</p>{/each}
  {/if}
  <p class="faint small">Look for several things at once by putting them apart with “;”.</p>
  {#if error}<p class="error small">{error}</p>{/if}

  {#if found}
    {#if !found.moments.length}
      <p class="muted">Nothing like that is said in what can be searched yet.</p>
    {:else}
      <div class="row wrap tools">
        <span class="muted small">{found.moments.length} found{found.by_meaning ? '' : ', by the words alone'}</span>
        <span class="grow"></span>
        <button class="btn small ghost" onclick={() => (picked = Object.fromEntries(found!.moments.map((m) => [key(m), chosen.length !== found!.moments.length])))}>
          {chosen.length === found.moments.length ? 'Select none' : 'Select all'}
        </button>
      </div>
      <ul class="moments">
        {#each found.moments as m (key(m))}
          <li class:picked={picked[key(m)]}>
            <label class="moment">
              <input class="tick" type="checkbox" bind:checked={picked[key(m)]} aria-label="Make a clip of {m.from} at {clock(m.start)}" />
              <div class="grow said">
                <div class="row wrap where">
                  <a class="link" href="#/title/{m.title_id}" onclick={(e) => e.stopPropagation()}>{m.from}</a>
                  <span class="faint mono">{clock(m.start)}</span>
                  {#if m.words}<span class="chip accent">the words</span>{/if}
                </div>
                {#each m.before as l}<p class="faint">{l.text}</p>{/each}
                {#each m.lines as l}<p class="line">{l.text}</p>{/each}
                {#each m.after as l}<p class="faint">{l.text}</p>{/each}
              </div>
            </label>
          </li>
        {/each}
      </ul>
    {/if}
  {/if}
</section>

{#if chosen.length}
  <div class="bulkbar" role="toolbar" aria-label="Make clips of the moments selected">
    <span class="n">{chosen.length} selected</span>
    <label class="small muted opt">From
      <select class="input small-select" bind:value={lead} aria-label="Seconds before the first line">
        {#each [0, 1, 2, 5, 10, 20, 30] as n}<option value={n}>{n} s before</option>{/each}
      </select>
    </label>
    <label class="small muted opt">To
      <select class="input small-select" bind:value={tail} aria-label="Seconds after the last line">
        {#each [0, 1.5, 3, 5, 10, 20, 30] as n}<option value={n}>{n} s after</option>{/each}
      </select>
    </label>
    <select class="input small-select" bind:value={subtitles} aria-label="Subtitles">
      <option value="auto">Subtitles when needed</option>
      <option value="on">Subtitles always</option>
      <option value="none">No subtitles</option>
    </select>
    <span class="sep"></span>
    <button class="btn small primary" onclick={make}>Make {chosen.length === 1 ? 'a clip' : `${chosen.length} clips`}</button>
  </div>
{/if}

<style>
  .finder {
    display: grid;
    gap: 8px;
    padding: 14px 16px;
    margin-bottom: 18px;
  }
  .look {
    gap: 8px;
  }
  .look .btn {
    flex-shrink: 0;
  }
  p {
    margin: 0;
  }
  .about .doing {
    display: block;
    margin-top: 2px;
  }
  .warn {
    color: var(--warn);
  }
  .error {
    color: var(--bad, #d9534f);
  }
  .tools {
    margin-top: 6px;
  }
  .moments {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 2px;
  }
  .moments li {
    border-radius: 10px;
  }
  .moments li.picked {
    background: var(--accent-soft);
  }
  .moment {
    display: flex;
    gap: 12px;
    align-items: flex-start;
    padding: 10px 8px;
    cursor: pointer;
  }
  .moment .tick {
    margin-top: 2px;
  }
  .said {
    min-width: 0;
    display: grid;
    gap: 2px;
    font-size: 14px;
  }
  .said p {
    overflow-wrap: anywhere;
    white-space: pre-line;
  }
  .where {
    gap: 8px;
    margin-bottom: 2px;
  }
  .line {
    font-weight: 600;
  }
  .opt {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
</style>
