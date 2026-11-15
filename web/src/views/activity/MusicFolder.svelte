<script lang="ts">
  // The music folders and their last reading, and how music apps connect. Which folder is a
  // location's music folder is set with the location, in Settings, Storage.
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

{#if status?.roots.length}
  <fieldset class="card">
    <legend>Music</legend>
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
    <p class="faint small">Music apps such as Amperfy, play:Sub, Feishin and Symfonium connect over OpenSubsonic to <span class="mono">{location.origin}</span>, signing in with a Spool name and password and the app's plain-password (legacy) sign-in.</p>
  </fieldset>
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
