<script lang="ts">
  // Where music plays: here, or on a player in the house such as a WiiM. Spool looks for
  // them on the network each time this is opened.
  import { api } from '../lib/api';
  import { player, playOn, type Renderer } from '../lib/player.svelte';

  // `compact`: on a phone, the button shows the speaker alone.
  let { up = false, compact = false }: { up?: boolean; compact?: boolean } = $props();
  let open = $state(false);
  let found = $state<Renderer[] | null>(null);
  let looking = $state(false);

  async function look() {
    open = !open;
    if (!open) return;
    looking = true;
    found = await api.get<Renderer[]>('/music/renderers?look=true').catch(() => []);
    looking = false;
  }
  async function choose(r: Renderer | null) {
    open = false;
    await playOn(r);
  }
</script>

<div class="playon" class:compact>
  <button class="btn small" class:on={!!player.on} onclick={look} aria-haspopup="menu" aria-expanded={open} title="Where music plays">
    <svg viewBox="0 0 24 24" width="15" height="15" aria-hidden="true"><path fill="currentColor" d="M6 3h12a1 1 0 0 1 1 1v16a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1zm6 7a4 4 0 1 0 0 8 4 4 0 0 0 0-8zm0 2a2 2 0 1 1 0 4 2 2 0 0 1 0-4zm0-7a1.5 1.5 0 1 0 0 3 1.5 1.5 0 0 0 0-3z" /></svg>
    <span class="label">{player.on ? player.on.name : 'This device'}</span>
  </button>
  {#if open}
    <div class="menu card" class:up role="menu">
      <button role="menuitem" class:sel={!player.on} onclick={() => choose(null)}>This device</button>
      {#each found ?? [] as r (r.id)}
        <button role="menuitem" class:sel={player.on?.id === r.id} onclick={() => choose(r)}>{r.name}{#if r.model && r.model !== r.name}<span class="faint small"> {r.model}</span>{/if}</button>
      {/each}
      {#if looking}<div class="faint small pad">Looking on the network…</div>
      {:else if found && !found.length}<div class="faint small pad">No players found on the network.</div>{/if}
    </div>
  {/if}
</div>

<style>
  .playon {
    position: relative;
  }
  .btn {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    max-width: 180px;
  }
  .btn.on {
    border-color: var(--accent);
    color: var(--accent);
  }
  .label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  @media (max-width: 560px) {
    .compact .label {
      display: none;
    }
  }
  .menu {
    position: absolute;
    right: 0;
    top: calc(100% + 6px);
    min-width: 220px;
    padding: 6px;
    display: grid;
    z-index: 40;
    box-shadow: 0 8px 28px rgb(0 0 0 / 0.3);
  }
  .menu.up {
    top: auto;
    bottom: calc(100% + 6px);
  }
  .menu button {
    text-align: left;
    padding: 8px 10px;
    border: 0;
    border-radius: 6px;
    background: none;
    color: inherit;
    font: inherit;
    cursor: pointer;
  }
  .menu button:hover {
    background: var(--raised);
  }
  .menu button.sel {
    color: var(--accent);
    font-weight: 600;
  }
  .pad {
    padding: 8px 10px;
  }
</style>
