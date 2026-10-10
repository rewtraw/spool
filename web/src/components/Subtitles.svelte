<script lang="ts">
  import { api } from '../lib/api';
  import { act, toast } from '../lib/state.svelte';

  // The subtitles of one video file: what is inside it, what sits beside it, and finding more.
  let { fileId }: { fileId: number } = $props();

  const NAMES: Record<string, string> = { en: 'English', es: 'Spanish', fr: 'French', de: 'German', it: 'Italian', pt: 'Portuguese', nl: 'Dutch', sv: 'Swedish', no: 'Norwegian', da: 'Danish', fi: 'Finnish', pl: 'Polish', ru: 'Russian', ja: 'Japanese', ko: 'Korean', zh: 'Chinese', ar: 'Arabic', he: 'Hebrew', tr: 'Turkish', el: 'Greek', cs: 'Czech', hu: 'Hungarian', ro: 'Romanian', uk: 'Ukrainian', hi: 'Hindi', th: 'Thai', vi: 'Vietnamese', id: 'Indonesian', und: 'Unknown language' };
  const name = (code: string) => NAMES[code] ?? code.toUpperCase();

  let info = $state<any>(null);
  let language = $state('en');
  let results = $state<any[] | null>(null);
  let busy = $state('');

  async function load() {
    info = await api.get(`/files/${fileId}/subtitles`).catch(() => info);
    if (info?.wanted?.length && !info.wanted.includes(language)) language = info.wanted[0];
  }
  $effect(() => {
    fileId;
    results = null;
    load();
  });

  function verdict(s: any): { label: string; tone: string; detail: string } {
    const r = s.sync;
    if (!r) return { label: 'Not checked', tone: '', detail: '' };
    const by = `${Math.abs(r.offset).toFixed(1)} s ${r.offset < 0 ? 'late' : 'early'}`;
    const how = r.method === 'subtitles' ? " Checked against the film's own subtitles." : r.method === 'sound' ? ' Checked by listening to the film.' : '';
    if (r.corrected) return { label: 'Corrected', tone: 'ok', detail: r.rate === 1 ? `It was ${by}; the file has been retimed.${how}` : `It was timed for a different frame rate; the file has been retimed.${how}` };
    if (r.verdict === 'in_sync') return { label: 'In time', tone: 'ok', detail: `Lines up with the speech in the film.${how}` };
    if (r.verdict === 'shifted') return { label: r.rate === 1 ? `${by}` : 'Wrong frame rate', tone: 'warn', detail: 'It belongs to this film but is out of time. Spool can correct it.' };
    if (r.verdict === 'no_match') return { label: 'Does not fit', tone: 'bad', detail: 'It does not line up with this soundtrack anywhere. It is probably for a different cut.' };
    return { label: 'Cannot tell', tone: '', detail: 'Too little speech to judge by.' };
  }

  async function check(s: any, correct: boolean) {
    busy = s.rel_path;
    const r = await act(() => api.post<any>(`/files/${fileId}/subtitles/check`, { rel_path: s.rel_path, correct }));
    busy = '';
    if (r) toast(r.corrected ? 'Retimed to match the film' : r.verdict === 'in_sync' ? 'In time with the film' : r.verdict === 'shifted' ? 'Out of time; it can be corrected' : r.verdict === 'no_match' ? 'These do not fit this film' : 'Could not tell');
    load();
  }
  async function remove(s: any) {
    if (!confirm(`Delete the ${name(s.language)} subtitle file?`)) return;
    await act(() => api.del(`/files/${fileId}/subtitles`, { rel_path: s.rel_path }));
    load();
  }
  async function find() {
    busy = 'find';
    results = (await act(() => api.post<any[]>(`/files/${fileId}/subtitles/search`, { language }))) ?? null;
    busy = '';
  }
  async function best() {
    busy = 'best';
    const r = await act(() => api.post<any>(`/files/${fileId}/subtitles/auto`, { language }));
    busy = '';
    if (r) toast(r.subtitle ? `${name(language)} subtitles added and checked against the film` : `No ${name(language)} subtitles were found that fit this film`);
    load();
  }
  async function add(c: any) {
    busy = `add${c.file_id}`;
    const r = await act(() => api.post<any>(`/files/${fileId}/subtitles/add`, c));
    busy = '';
    if (r) {
      const v = r.report;
      toast(v.corrected ? 'Added, and retimed to match the film' : v.verdict === 'in_sync' ? 'Added. In time with the film.' : v.verdict === 'no_match' ? 'Added, but these do not fit this film' : v.verdict === 'shifted' ? 'Added, but out of time' : 'Added. Could not check the timing.');
      results = null;
    }
    load();
  }
</script>

{#if info}
  <div class="subs">
    <div class="row wrap line">
      <span class="faint label">Subtitles</span>
      {#each info.embedded as l}<span class="chip" title="Inside the video file">{name(l)}</span>{/each}
      {#if !info.embedded.length && !info.beside.length}<span class="faint">None</span>{/if}
    </div>
    {#each info.beside as s (s.rel_path)}
      {@const v = verdict(s)}
      <div class="row wrap line">
        <span class="chip accent">{name(s.language)}{s.hearing_impaired ? ' · SDH' : ''}{s.forced ? ' · forced' : ''}</span>
        <span class="chip {v.tone}" title={v.detail}>{v.label}</span>
        <span class="faint small grow truncate" title={s.rel_path}>{s.source === 'opensubtitles' ? 'OpenSubtitles' : s.source === 'download' ? 'Came with the release' : 'Separate file'}{s.release ? ` · ${s.release}` : ''}</span>
        {#if info.can_check}
          {#if s.sync?.verdict === 'shifted' && !s.sync?.corrected}<button class="btn small" disabled={!!busy} onclick={() => check(s, true)}>{busy === s.rel_path ? 'Listening…' : 'Correct timing'}</button>
          {:else}<button class="btn small ghost" disabled={!!busy} onclick={() => check(s, false)}>{busy === s.rel_path ? 'Listening…' : 'Check timing'}</button>{/if}
        {/if}
        <button class="btn small ghost danger" disabled={!!busy} onclick={() => remove(s)}>Delete</button>
      </div>
    {/each}
    {#if info.can_search}
      <div class="row wrap line">
        <select class="input lang" bind:value={language} aria-label="Language">
          {#each Object.keys(NAMES).filter((k) => k !== 'und') as k}<option value={k}>{NAMES[k]}</option>{/each}
        </select>
        <button class="btn small" disabled={!!busy} onclick={best} title="Take the best match, check it against the film, and try the next if it does not fit">{busy === 'best' ? 'Finding and checking…' : 'Find and check'}</button>
        <button class="btn small ghost" disabled={!!busy} onclick={find}>{busy === 'find' ? 'Searching…' : 'Choose from a list'}</button>
      </div>
      {#if results}
        {#if !results.length}<div class="faint small">Nothing found in {name(language)}.</div>{/if}
        {#each results.slice(0, 12) as c (c.file_id)}
          <div class="row line cand">
            <div class="grow" style="min-width:0">
              <div class="mono small break">{c.release || 'Unnamed'}</div>
              <div class="faint small">{c.downloads.toLocaleString()} downloads{c.hash_match ? ' · timed for this exact file' : c.same_release ? ' · made for this release' : ''}{c.hearing_impaired ? ' · SDH' : ''}{c.machine_translated ? ' · machine translated' : ''}</div>
            </div>
            <button class="btn small" disabled={!!busy} onclick={() => add(c)}>{busy === `add${c.file_id}` ? 'Checking…' : 'Use'}</button>
          </div>
        {/each}
      {/if}
    {/if}
  </div>
{/if}

<style>
  .subs {
    display: grid;
    gap: 7px;
    margin-top: 10px;
    padding-top: 10px;
    border-top: 1px solid var(--line);
  }
  .line {
    gap: 7px;
    align-items: center;
  }
  .label {
    font-size: 12.5px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }
  .lang {
    width: auto;
    height: 30px;
    padding: 0 8px;
    font-size: 13.5px;
  }
  .cand {
    padding: 7px 10px;
    border-radius: 8px;
    background: var(--raised);
  }
  .small {
    font-size: 12.5px;
  }
</style>
