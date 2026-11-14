<script lang="ts">
  // The Music tab: the music folders and their last reading, and how music players connect.
  // Which folder is a location's music folder is set with the location, in Library.
  import { api } from '../../lib/api';
  import { toast } from '../../lib/state.svelte';
  import { ago, bytes } from '../../lib/format';

  let status = $state<any>(null);
  async function load() {
    status = (await api.get<{ status: any }>('/music').catch(() => null))?.status ?? status;
  }
  load();
  $effect(() => {
    if (!status?.scan?.scanning) return;
    const t = setInterval(load, 3000);
    return () => clearInterval(t);
  });
  async function scan() {
    try {
      await api.post('/music/scan', {});
      status = { ...status, scan: { ...status?.scan, scanning: true } };
    } catch (e: any) {
      toast(e.message);
    }
  }
</script>

{#if !status}
  <div class="empty">Loading…</div>
{:else}
  <div class="stack form">
    <fieldset class="card">
      <legend>The music folder</legend>
      {#if status.roots.length}
        <div class="row between">
          <div class="grow">
            <b>{status.albums.toLocaleString()}</b> <span class="muted">albums · {status.tracks.toLocaleString()} tracks · {bytes(status.bytes)}</span>
            <div class="faint small">
              {#if status.scan.scanning}Reading the music folders… {status.scan.read ? `${status.scan.read} files read` : ''}
              {:else if status.scan.last_at}Read {ago(status.scan.last_at)}, and again every six hours{/if}
            </div>
          </div>
          <button class="btn small" disabled={status.scan.scanning} onclick={scan}>{status.scan.scanning ? 'Reading…' : 'Read again'}</button>
        </div>
        {#each status.roots as r}<div class="mono small">{r.path}{#if !r.online}<span class="warn"> (cannot be reached)</span>{/if}</div>{/each}
        {#if status.scan.error}<div class="warn small">The last read stopped: {status.scan.error}</div>{/if}
      {:else}
        <p class="muted">No music folder yet. Give a location one in <a class="link" href="#/settings/library">Library</a>.</p>
      {/if}
      <p class="faint small">Albums wanted are filed here as “Artist - Album (Year) [FLAC]”, in the first music folder Spool manages.</p>
    </fieldset>
    <fieldset class="card">
      <legend>Music players</legend>
      <p class="muted">Apps such as Amperfy, play:Sub, Feishin and Symfonium connect over OpenSubsonic.</p>
      <div class="field">Server <span class="mono">{location.origin}</span></div>
      <p class="faint small">Sign in with your Spool name and password, and turn on the app's plain-password (legacy) sign-in.</p>
    </fieldset>
  </div>
{/if}

<style>
  .between {
    justify-content: space-between;
    align-items: center;
    gap: 12px;
  }
  .grow {
    flex: 1;
    min-width: 0;
  }
  .warn {
    color: var(--warn);
  }
</style>
