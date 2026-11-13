<script lang="ts">
  // The Books tab: the Anna's Archive key books are downloaded with, where they go, and the
  // languages they are read in.
  import { api } from '../../lib/api';
  import { act } from '../../lib/state.svelte';

  let general = $state<any>(null);
  async function load() {
    general = await api.get('/settings/general');
  }
  load().catch(() => {});
  async function save(e: Event) {
    e.preventDefault();
    await act(() => api.put('/settings/general', general), 'Saved');
    load();
  }
</script>

{#if general}
  <form class="stack form" onsubmit={save}>
    <fieldset class="card">
      <legend>Anna's Archive</legend>
      <p class="muted">Books are found in an index Spool builds from the metadata Anna's Archive publishes, and downloaded with a member's key. Each download is checked against the file it was asked for.</p>
      <label class="field">Secret key<input class="input mono" bind:value={general.books.annas_key} autocomplete="off" /><span class="hint">From your account page on Anna's Archive, once you are a member. Each membership allows a number of downloads a day.</span></label>
      <label class="field narrow">Languages<input class="input" bind:value={general.books.languages} placeholder="en" autocomplete="off" /><span class="hint">Two-letter codes, separated by commas: <code>en</code>, or <code>en, fr</code>. The index keeps books in these alone; build it again after changing them.</span></label>
    </fieldset>
    <fieldset class="card">
      <legend>Florilegium</legend>
      <p class="muted">Each book Spool gets is put on Florilegium's shelf and in its library, and marked as got on its reading list. Without it, books are kept in Spool to download.</p>
      <label class="field">Address<input class="input mono" bind:value={general.books.florilegium_url} placeholder="http://melchior:8790" autocomplete="off" /></label>
      <label class="field">Token<input class="input mono" type="password" bind:value={general.books.florilegium_token} autocomplete="new-password" /><span class="hint">The token Florilegium's server is started with.</span></label>
    </fieldset>
    <div><button class="btn primary">Save</button></div>
  </form>
{:else}
  <div class="empty">Loading…</div>
{/if}
