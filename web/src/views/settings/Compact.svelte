<script lang="ts">
  // Compact versions: how they are made, where those of read-only titles go, and whether
  // whatever arrives gets one. How the library stands is on Activity's Space page.
  import { api } from '../../lib/api';
  import { bytes } from '../../lib/format';
  import { conf, saveSettings } from '../../lib/settings.svelte';

  let places = $state<any[]>([]);
  let means = $state('');
  api.get<any[]>('/locations').then((p) => (places = p)).catch(() => {});
  api.get<any>('/compact').then((r) => (means = r.means ?? '')).catch(() => {});
  const c = $derived(conf.g?.compact);
  const managed = $derived(places.filter((p) => p.id && p.manage));
  const readOnly = $derived(places.filter((p) => p.id && !p.manage));
  // A film of two hours at these settings, to give the numbers some meaning.
  const twoHours = $derived(c ? ((c.video_kbps + c.audio_kbps) * 1000 * 7200) / 8 : 0);
  const PRESETS: [number, number, string][] = [[1080, 8000, '1080p, 8 Mb/s'], [1080, 4000, '1080p, 4 Mb/s'], [2160, 20000, '4K, 20 Mb/s'], [720, 3000, '720p, 3 Mb/s']];

  async function save() {
    const x = conf.g.compact;
    await saveSettings({ compact: { height: Number(x.height) || 0, video_kbps: Number(x.video_kbps) || 0, audio_kbps: Number(x.audio_kbps) || 0, location: Number(x.location) || 0, software: x.software, auto: x.auto } });
  }
</script>

{#if c}
  <form onsubmit={(e) => { e.preventDefault(); save(); }}>
    <fieldset class="card">
      <legend>Compact versions</legend>
      <p class="muted">A smaller copy of a film or an episode, kept beside the original, for playing away from home or over a slow connection. The original is never touched; a player is offered both. <span class:warn={means && !means.includes('hardware')}>{means ? `Made with ${means}.` : ''}</span></p>
      <div class="row wrap">
        {#each PRESETS as [h, k, label]}<button class="btn small" type="button" class:primary={c.height === h && c.video_kbps === k} onclick={() => ((c.height = h), (c.video_kbps = k))}>{label}</button>{/each}
      </div>
      <span class="hint">A two-hour film comes to about {bytes(twoHours)}. HEVC, ten-bit where the original is, HDR10 kept; subtitles, chapters and fonts carried over.</span>
      <details>
        <summary class="small">Exact sizes</summary>
        <div class="cols">
          <label class="field">Picture height<input class="input" type="number" min="240" max="4320" step="1" bind:value={c.height} /></label>
          <label class="field">Picture, kilobits a second<input class="input" type="number" min="200" max="80000" step="100" bind:value={c.video_kbps} /></label>
          <label class="field">Surround sound, kilobits a second<input class="input" type="number" min="128" max="1536" step="32" bind:value={c.audio_kbps} /><span class="hint">Sound already small is copied as it is.</span></label>
        </div>
      </details>
      <label class="check"><input type="checkbox" bind:checked={c.auto} /> Make one of whatever arrives, when it would gain from one</label>
      <label class="check"><input type="checkbox" bind:checked={c.software} /> Use the processor when the video hardware cannot do a file</label>
      {#if readOnly.length}
        <label class="field narrow">For titles on {readOnly.map((p) => p.name).join(' and ')}, which Spool only reads
          <select class="input" bind:value={c.location}>
            <option value={0}>Make none</option>
            {#each managed as p}<option value={p.id}>Keep them on {p.name}{p.free != null ? ` (${bytes(p.free)} free)` : ''}</option>{/each}
          </select>
        </label>
      {/if}
      <div><button class="btn primary">Save</button></div>
    </fieldset>
  </form>
{/if}
