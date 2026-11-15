<script lang="ts">
  // Usenet providers, and the speed limit. A provider needs its address and sign-in; the
  // rest has the values nearly every provider wants, under More.
  import { api } from '../../lib/api';
  import { act, loadStatus } from '../../lib/state.svelte';

  let servers = $state<any[]>([]);
  let speedLimit = $state(0);
  let tests = $state<Record<string, { ok: boolean; message: string } | 'running'>>({});
  async function load() {
    const r = await api.get('/settings/servers');
    servers = r.servers;
    speedLimit = Math.round(r.speed_limit / 1024 / 1024);
  }
  load().catch(() => {});

  function addServer() {
    servers.push({ id: '', name: '', host: '', port: 563, tls: true, tls_verify: true, username: '', password: '', connections: 20, priority: servers.length, enabled: true, pipeline: 2 });
  }
  const numbered = (s: any) => ({ ...s, port: Number(s.port), connections: Number(s.connections), priority: Number(s.priority), pipeline: Number(s.pipeline) });
  async function saveServers() {
    await act(() => api.put('/settings/servers', { servers: servers.map(numbered), speed_limit: Math.max(0, Number(speedLimit)) * 1024 * 1024 }), 'Saved');
    loadStatus();
    load();
  }
  async function testServer(s: any, i: number) {
    tests[`s${i}`] = 'running';
    tests[`s${i}`] = await api.post('/servers/test', numbered(s)).catch((e) => ({ ok: false, message: e.message }));
  }
</script>

<fieldset class="card">
  <legend>Usenet</legend>
  {#each servers as s, i}
    <div class="place">
      <div class="row wrap">
        <b>{s.name || s.host || 'A new provider'}</b>
        {#if !s.enabled}<span class="small warn">turned off</span>{/if}
        <span class="grow"></span>
        {#if tests[`s${i}`]}<span class="result" class:bad={tests[`s${i}`] !== 'running' && !(tests[`s${i}`] as any).ok}>{tests[`s${i}`] === 'running' ? 'Testing…' : (tests[`s${i}`] as any).message}</span>{/if}
        <button class="btn small" type="button" onclick={() => testServer(s, i)}>Test</button>
      </div>
      <div class="cols">
        <label class="field">Address<input class="input mono" bind:value={s.host} placeholder="news.example.com" /></label>
        <label class="field">Username<input class="input mono" bind:value={s.username} autocomplete="off" /></label>
        <label class="field">Password<input class="input mono" type="password" bind:value={s.password} autocomplete="new-password" /></label>
      </div>
      <details>
        <summary class="small">More</summary>
        <div class="cols">
          <label class="field">Port<input class="input" type="number" bind:value={s.port} /></label>
          <label class="field">Connections<input class="input" type="number" min="1" max="100" bind:value={s.connections} /></label>
          <label class="field">Order<input class="input" type="number" min="0" bind:value={s.priority} /><span class="hint">0 is asked first; the others only fill in what it lacks.</span></label>
        </div>
        <div class="row wrap">
          <label class="check"><input type="checkbox" bind:checked={s.enabled} /> Use it</label>
          <label class="check"><input type="checkbox" bind:checked={s.tls} /> Encrypted</label>
          <label class="check"><input type="checkbox" bind:checked={s.tls_verify} disabled={!s.tls} /> Check its certificate</label>
          <span class="grow"></span>
          <button class="btn small ghost danger" type="button" onclick={() => servers.splice(i, 1)}>Remove</button>
        </div>
      </details>
    </div>
  {/each}
  {#if !servers.length}<p class="small warn">No provider yet. Spool cannot download without one.</p>{/if}
  <label class="field narrow">Speed limit (MB a second)<input class="input" type="number" min="0" bind:value={speedLimit} /><span class="hint">0 is no limit.</span></label>
  <div class="row"><button class="btn" type="button" onclick={addServer}>Add a provider</button><button class="btn primary" type="button" onclick={saveServers}>Save</button></div>
</fieldset>
