<script lang="ts">
  // The Compact versions tab: how they are made, where they go, and how the library stands.
  import { api } from '../../lib/api';
  import { act, toast } from '../../lib/state.svelte';
  import { bytes, duration } from '../../lib/format';

  let general = $state<any>(null);
  let report = $state<any>(null);
  let places = $state<any[]>([]);
  let asking = $state(false);
  const c = $derived(general?.compact);
  const managed = $derived(places.filter((p) => p.id && p.manage));
  const readOnly = $derived(places.filter((p) => p.id && !p.manage));
  const making = $derived(report?.making?.now?.length > 0 || report?.making?.waiting_count > 0);
  // A film of two hours at these settings, to give the numbers some meaning.
  const twoHours = $derived(c ? ((c.video_kbps + c.audio_kbps) * 1000 * 7200) / 8 : 0);

  async function load() {
    [general, places] = await Promise.all([api.get<any>('/settings/general'), api.get<any[]>('/locations').catch(() => [])]);
    loadReport();
  }
  async function loadReport() {
    report = await api.get<any>('/compact').catch(() => report);
  }
  $effect(() => {
    load();
  });
  $effect(() => {
    if (!making) return;
    const timer = setInterval(loadReport, 5000);
    return () => clearInterval(timer);
  });

  function preset(height: number, kbps: number) {
    general.compact.height = height;
    general.compact.video_kbps = kbps;
  }
  async function save() {
    for (const k of ['height', 'video_kbps', 'audio_kbps', 'location']) general.compact[k] = Number(general.compact[k]) || 0;
    await act(() => api.put('/settings/general', general), 'Saved');
    load();
  }
  async function everything() {
    const w = report?.would;
    if (!w?.files) return;
    if (!confirm(`Make ${w.files} compact ${w.files === 1 ? 'version' : 'versions'}?\n\nThey will take about ${bytes(w.bytes)} and are made one at a time in the background. The originals are not touched. The line can be cancelled in Activity at any time.`)) return;
    asking = true;
    const r = await act(() => api.post<any>('/compact/everything'));
    asking = false;
    if (r) toast(`${r.queued} in line. Progress is in Activity.`);
    loadReport();
  }
  async function cancelAll() {
    await act(() => api.post('/compact/cancel', {}), 'The waiting compact versions are cancelled');
    loadReport();
  }
</script>

{#if !general}
  <div class="empty">Loading…</div>
{:else}
  <form class="stack" onsubmit={(e) => { e.preventDefault(); save(); }}>
    <fieldset class="card">
      <legend>Compact versions</legend>
      <p class="muted">A compact version is a smaller copy of a film or an episode that Spool makes and keeps beside the original, for playing away from home or over a slow connection. The original is never touched. A player is offered both: in Infuse, hold Play and choose <b>Version</b>.</p>
      <p class="small" class:warn={report && !report.means?.includes('hardware')}>Made with {report?.means ?? '…'}.</p>
    </fieldset>

    <fieldset class="card">
      <legend>How they are made</legend>
      <div class="row wrap">
        <button class="btn small" type="button" class:primary={c.height === 1080 && c.video_kbps === 8000} onclick={() => preset(1080, 8000)}>1080p, 8 Mb/s</button>
        <button class="btn small" type="button" class:primary={c.height === 1080 && c.video_kbps === 4000} onclick={() => preset(1080, 4000)}>1080p, 4 Mb/s</button>
        <button class="btn small" type="button" class:primary={c.height === 2160 && c.video_kbps === 20000} onclick={() => preset(2160, 20000)}>4K, 20 Mb/s</button>
        <button class="btn small" type="button" class:primary={c.height === 720 && c.video_kbps === 3000} onclick={() => preset(720, 3000)}>720p, 3 Mb/s</button>
      </div>
      <div class="cols">
        <label class="field">Picture height<input class="input" type="number" min="240" max="4320" step="1" bind:value={general.compact.height} /><span class="hint">The picture is made to fit a screen this high: 1080 fits within 1920 by 1080. A picture that fits already is not made bigger.</span></label>
        <label class="field">Picture, kilobits a second<input class="input" type="number" min="200" max="80000" step="100" bind:value={general.compact.video_kbps} /><span class="hint">A two-hour film comes to about {bytes(twoHours)} at these settings.</span></label>
      </div>
      <label class="field narrow">Surround sound, kilobits a second<input class="input" type="number" min="128" max="1536" step="32" bind:value={general.compact.audio_kbps} /><span class="hint">Sound that is already small (AAC, Dolby Digital, Dolby Digital Plus) is copied as it is. The rest, such as TrueHD and DTS-HD, becomes Dolby Digital Plus at this rate, in 5.1.</span></label>
      <p class="hint">The picture is HEVC, ten-bit where the original is, with HDR10 kept. Dolby Vision over an HDR10 layer comes out as HDR10. Subtitles, chapters and fonts are carried over. A file less than half again as big as its compact version would be is left alone.</p>
      <label class="check"><input type="checkbox" bind:checked={general.compact.software} /> Use the processor when the video hardware cannot do a file</label>
      <span class="hint">Much slower, and it keeps the processor busy. Off, such a file is reported and left.</span>
    </fieldset>

    <fieldset class="card">
      <legend>Where and when</legend>
      <p class="muted">A compact version is kept beside its original wherever Spool may write.</p>
      {#if readOnly.length}
        <label class="field narrow">For titles on {readOnly.map((p) => p.name).join(' and ')}, which Spool only reads
          <select class="input" bind:value={general.compact.location}>
            <option value={0}>Make none</option>
            {#each managed as p}<option value={p.id}>Keep them on {p.name}{p.free != null ? ` (${bytes(p.free)} free)` : ''}</option>{/each}
          </select>
          <span class="hint">They are put in a folder of the title's name there, which becomes one more place the title is held.</span>
        </label>
      {/if}
      <label class="check"><input type="checkbox" bind:checked={general.compact.auto} /> Make one of whatever arrives, when it would gain from one</label>
      <span class="hint">Each download that is filed is put in line. What is already in the library is not; use the button below for that.</span>
    </fieldset>

    <div><button class="btn primary" type="submit">Save</button></div>
  </form>

  <div class="stack" style="margin-top:24px">
  <fieldset class="card">
    <legend>The library</legend>
    {#if !report}
      <p class="muted">Working it out…</p>
    {:else}
      <p><b>{report.made.files}</b> compact {report.made.files === 1 ? 'version' : 'versions'}{#if report.made.files}, {bytes(report.made.bytes)} beside {bytes(report.made.original_bytes)} of originals{/if}.</p>
      {#if making}
        <p class="muted">{report.making.now.length ? `Making ${report.making.now.map((n: any) => `${n.title}${n.episode ? ` ${n.episode}` : ''}${n.by ? ` on ${n.by}` : ''}`).join(', ')}` : 'Starting'}{report.making.waiting_count ? `, with ${report.making.waiting_count} waiting` : ''}. <a class="link" href="#/activity">Progress is in Activity</a>.</p>
        {#if report.making.waiting_count}<div><button class="btn small ghost" type="button" onclick={cancelAll}>Cancel all waiting</button></div>{/if}
      {/if}
      {#if report.would.files}
        <p style="margin-top:12px"><b>{report.would.files}</b> more {report.would.files === 1 ? 'file' : 'files'} would gain from one at the saved settings: about {bytes(report.would.bytes)} from {bytes(report.would.original_bytes)}, and {duration(report.would.running_secs)} of film to get through.</p>
        <div><button class="btn" type="button" disabled={asking} onclick={everything}>{asking ? 'Putting them in line…' : `Make all ${report.would.files}`}</button></div>
      {:else if !making}
        <p class="muted" style="margin-top:12px">Nothing else in the library would gain from one at the saved settings.</p>
      {/if}
      {#if Object.keys(report.why_not ?? {}).length}
        <details style="margin-top:14px">
          <summary>Files that get none, and why</summary>
          <div class="list">
            {#each Object.entries(report.why_not) as [why, n]}<div class="item"><b>{n}</b> <span class="muted">{why}</span></div>{/each}
          </div>
        </details>
      {/if}
    {/if}
  </fieldset>
  </div>
{/if}
