<script lang="ts">
  // Settings: the tabs, and the one that is open. Each tab is a component beside this file's
  // folder, with its own state; what they share in looks is in settings/settings.css.
  import { route } from '../lib/state.svelte';
  import './settings/settings.css';
  import General from './settings/General.svelte';
  import Usenet from './settings/Usenet.svelte';
  import Indexers from './settings/Indexers.svelte';
  import Profiles from './settings/Profiles.svelte';
  import Naming from './settings/Naming.svelte';
  import Plex from './settings/Plex.svelte';
  import Space from './settings/Space.svelte';
  import Compact from './settings/Compact.svelte';
  import Maintenance from './settings/Maintenance.svelte';

  const tabs = [['general', 'General'], ['usenet', 'Usenet'], ['indexers', 'Indexers'], ['profiles', 'Profiles'], ['naming', 'Naming'], ['plex', 'Plex'], ['compact', 'Compact versions'], ['space', 'Disk space'], ['maintenance', 'Maintenance']];
  const tab = $derived(route.arg || 'general');
</script>

<div class="settings">
  <header class="head"><h1>Settings</h1></header>
  <div class="tabs" role="tablist">
    {#each tabs as [id, label]}<a role="tab" aria-selected={tab === id} class:on={tab === id} href="#/settings/{id}">{label}</a>{/each}
  </div>

  {#if tab === 'general'}<General />
  {:else if tab === 'usenet'}<Usenet />
  {:else if tab === 'indexers'}<Indexers />
  {:else if tab === 'profiles'}<Profiles />
  {:else if tab === 'naming'}<Naming />
  {:else if tab === 'plex'}<Plex />
  {:else if tab === 'compact'}<Compact />
  {:else if tab === 'space'}<Space />
  {:else if tab === 'maintenance'}<Maintenance />
  {/if}
</div>
