<script lang="ts">
  // Settings: where the library is, what the household wants, what Spool connects to, and who
  // uses it. Reports and tasks are under Activity. Each tab is a component in settings/.
  import { route } from '../lib/state.svelte';
  import '../lib/panes.css';
  import Storage from './settings/Storage.svelte';
  import Preferences from './settings/Preferences.svelte';
  import Connections from './settings/Connections.svelte';
  import People from './settings/People.svelte';

  const tabs = [['storage', 'Storage'], ['preferences', 'Preferences'], ['connections', 'Connections'], ['people', 'People']];
  const tab = $derived(tabs.some(([id]) => id === route.arg) ? route.arg : 'storage');
</script>

<div class="panes">
  <header class="head"><h1>Settings</h1></header>
  <div class="tabs" role="tablist">
    {#each tabs as [id, label]}<a role="tab" aria-selected={tab === id} class:on={tab === id} href="#/settings/{id}">{label}</a>{/each}
  </div>

  {#if tab === 'storage'}<Storage />
  {:else if tab === 'preferences'}<Preferences />
  {:else if tab === 'connections'}<Connections />
  {:else if tab === 'people'}<People />
  {/if}
</div>
