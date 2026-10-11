<script lang="ts">
  // The Plex tab: what each library has that the other lacks.
  import { api } from '../../lib/api';
  import { act, toast } from '../../lib/state.svelte';
  import { ago } from '../../lib/format';

  // ---- plex
  let plex = $state<any>(null);
  let plexBusy = $state(false);
  async function loadPlex() {
    plex = await api.get('/plex').catch(() => null);
  }
  async function syncPlex() {
    plexBusy = true;
    const r = await act(() => api.post<any>('/plex/sync'));
    plexBusy = false;
    if (r) toast(r.message);
    loadPlex();
  }
  async function trackPlex(item: any) {
    const t = await act(() => api.post<any>('/plex/track', { rating_key: item.rating_key }), `Now tracking ${item.title}`);
    if (t) loadPlex();
  }
  async function rescan(item: any) {
    await act(() => api.post(`/titles/${item.title_id}/scan`), 'Rescanned');
    loadPlex();
  }

  loadPlex();
</script>

  <div class="stack form">
    {#if plex && !plex.configured}
      <div class="card empty">Plex is not set up. Add its address and token under <a class="link" href="#/settings/general">General</a>.</div>
    {:else if plex}
      <fieldset class="card">
        <legend>Library match</legend>
        <p class="muted">
          {#if plex.synced_at}Plex was last read {ago(plex.synced_at)}: <b>{plex.plex_items}</b> films and shows, <b>{plex.matched}</b> matched to titles in Spool.{:else}Plex has not been read yet.{/if}
          Matched titles get an Open in Plex link on their page. Spool reads Plex once an hour.
        </p>
        <div><button class="btn" disabled={plexBusy} onclick={syncPlex}>{plexBusy ? 'Reading Plex…' : 'Read Plex now'}</button></div>
      </fieldset>
      <fieldset class="card">
        <legend>In Plex, not in Spool ({plex.plex_only.length})</legend>
        {#if !plex.plex_only.length}<p class="muted">Everything in Plex is tracked by Spool.</p>
        {:else}
          <p class="muted">Tracking adds the title to Spool unmonitored, pointing at the folder Plex plays it from. Nothing is moved, renamed or downloaded.</p>
          <div class="list">
            {#each plex.plex_only as it (it.rating_key)}
              <div class="task">
                <div class="grow"><b>{it.title}</b> <span class="muted">{it.year || ''}</span><div class="faint small">{it.section}{it.path ? ` · ${it.path}` : ''}</div></div>
                {#if it.url}<a class="btn small ghost" href={it.url} target="_blank" rel="noopener noreferrer">Open in Plex</a>{/if}
                {#if it.can_track}<button class="btn small" onclick={() => trackPlex(it)}>Track in Spool</button>{:else}<span class="faint small">Plex has no id for it</span>{/if}
              </div>
            {/each}
          </div>
        {/if}
      </fieldset>
      <fieldset class="card">
        <legend>In Spool with files, not in Plex ({plex.spool_only.length})</legend>
        {#if !plex.spool_only.length}<p class="muted">Every title with files was found in Plex.</p>
        {:else}
          <p class="muted">Plex may not have scanned these yet, may have matched them to the wrong film, or the files may be gone. Rescanning checks the folder on disk.</p>
          <div class="list">
            {#each plex.spool_only as it (it.title_id)}
              <div class="task">
                <div class="grow"><a class="link" href="#/title/{it.title_id}"><b>{it.title}</b></a> <span class="muted">{it.year || ''}</span><div class="faint small break">{it.path}</div></div>
                <button class="btn small" onclick={() => rescan(it)}>Rescan folder</button>
              </div>
            {/each}
          </div>
        {/if}
      </fieldset>
    {/if}
  </div>
