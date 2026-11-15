<script lang="ts">
  // Preferences: what the household wants, in its own words. Languages, subtitles, the
  // profiles new titles get, the profiles themselves, and compact versions.
  import { app } from '../../lib/state.svelte';
  import { conf, loadSettings, saveSettings, pick } from '../../lib/settings.svelte';
  import Profiles from './Profiles.svelte';
  import Compact from './Compact.svelte';

  loadSettings();
  async function save() {
    await saveSettings(pick(conf.g, ['language', 'subtitle_languages', 'subtitles_auto', 'default_movie_profile', 'default_series_profile'], ['default_movie_profile', 'default_series_profile']));
  }
</script>

<div class="stack form">
  {#if conf.g}
    {@const g = conf.g}
    <form onsubmit={(e) => { e.preventDefault(); save(); }}>
      <fieldset class="card">
        <legend>Language and subtitles</legend>
        <label class="field narrow">Language<input class="input" bind:value={g.language} placeholder="english" /><span class="hint">Releases that name another language are turned down, unless it is the one the film or series was made in. Empty takes any.</span></label>
        <label class="field narrow">Subtitles in<input class="input" bind:value={g.subtitle_languages} placeholder="en" autocomplete="off" /><span class="hint">Codes or names: <code>en</code>, or <code>en, es</code>. A file with them inside or beside it needs nothing more.</span></label>
        <label class="check"><input type="checkbox" bind:checked={g.subtitles_auto} /> Look for missing subtitles by itself</label>
        <span class="hint">On OpenSubtitles (under Connections), a few files every six hours. Each subtitle is checked against the speech and retimed or thrown away.</span>
        <div class="cols">
          <label class="field">Profile for new films
            <select class="input" bind:value={g.default_movie_profile}>
              {#each app.profiles.filter((p) => p.kind === 'movie') as p}<option value={p.id}>{p.name}</option>{/each}
            </select>
          </label>
          <label class="field">Profile for new series
            <select class="input" bind:value={g.default_series_profile}>
              {#each app.profiles.filter((p) => p.kind === 'tv') as p}<option value={p.id}>{p.name}</option>{/each}
            </select>
          </label>
        </div>
        <div><button class="btn primary">Save</button></div>
      </fieldset>
    </form>
  {/if}
  <Profiles />
  <Compact />
</div>
