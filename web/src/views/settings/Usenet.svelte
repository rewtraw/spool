<script lang="ts">
  // The Usenet tab: providers and the speed limit.
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
</script>

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
