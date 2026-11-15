<script lang="ts">
  // How each profile fits the files on disk, location by location.
  import { api } from '../../lib/api';
  import { bytes } from '../../lib/format';

  let fit = $state<any[]>([]);
  api.get<any[]>('/profiles/fit').then((f) => (fit = f)).catch(() => {});
</script>

{#if fit.length}
  <fieldset class="card">
    <legend>How the profiles fit what is on disk</legend>
    <p class="muted">Where a title's files are not what its profile says, nothing happens while it is not monitored; once it is, Spool works to make them so. <a class="link" href="#/settings/preferences">Profiles</a></p>
    <div class="scroll-x">
      <table class="table">
        <thead><tr><th>Location</th><th>Profile</th><th>Titles</th><th>Monitored</th><th>Files</th><th>Quality not allowed</th><th>Over size target</th><th>Would upgrade</th></tr></thead>
        <tbody>
          {#each fit as f}
            <tr>
              <td>{f.location || 'Library'}</td><td><b>{f.profile}</b> <span class="faint small">{f.kind}</span></td><td>{f.titles}</td><td>{f.monitored}</td><td>{f.files}</td>
              <td class:warn={f.not_allowed > 0}>{f.not_allowed || ''}</td><td class:warn={f.over_target > 0}>{f.over_target ? `${f.over_target} (${bytes(f.over_bytes)})` : ''}</td><td>{f.below_cutoff || ''}</td>
            </tr>
            {#if f.note}<tr><td colspan="8" class="small warn">{f.note}</td></tr>{/if}
          {/each}
        </tbody>
      </table>
    </div>
  </fieldset>
{/if}
