<script lang="ts">
  // The Naming tab: how imported files and folders are named.
  import { api } from '../../lib/api';
  import { act } from '../../lib/state.svelte';

  let naming = $state<any>(null);
  api.get('/settings/naming').then((r) => (naming = r)).catch(() => {});
</script>

{#if naming}
  <form class="stack form" onsubmit={async (e) => { e.preventDefault(); await act(() => api.put('/settings/naming', naming), 'Saved'); }}>
    <fieldset class="card">
      <legend>Renaming</legend>
      <label class="check"><input type="checkbox" bind:checked={naming.rename} /> Rename files when importing</label>
      <label class="check"><input type="checkbox" bind:checked={naming.replace_illegal_characters} /> Replace characters a file name cannot hold</label>
      <p class="hint">These formats use the same tokens as Radarr and Sonarr, so existing files keep their names.</p>
    </fieldset>
    <fieldset class="card">
      <legend>Movies</legend>
      <label class="field">Folder<input class="input mono" bind:value={naming.movie_folder_format} /></label>
      <label class="field">File<textarea class="input" rows="3" bind:value={naming.movie_file_format}></textarea></label>
    </fieldset>
    <fieldset class="card">
      <legend>Series</legend>
      <label class="field">Series folder<input class="input mono" bind:value={naming.series_folder_format} /></label>
      <label class="field">Season folder<input class="input mono" bind:value={naming.season_folder_format} /></label>
      <label class="field">Episode file<textarea class="input" rows="2" bind:value={naming.episode_file_format}></textarea></label>
    </fieldset>
    <div><button class="btn primary" type="submit">Save</button></div>
  </form>
{/if}
