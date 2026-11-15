<script lang="ts">
  // Quality profiles: what a title may be downloaded in, where upgrades stop, and the size
  // to aim for.
  import { api } from '../../lib/api';
  import { act, loadProfiles } from '../../lib/state.svelte';

  let profiles = $state<any[]>([]);
  let qualities = $state<any[]>([]);
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

  async function load() {
    profiles = await api.get('/profiles');
    qualities = await api.get('/qualities');
  }
  load().catch(() => {});
</script>

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
  </div>
