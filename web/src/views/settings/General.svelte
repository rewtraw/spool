<script lang="ts">
  // The General tab: mode, downloads, how releases are chosen, connections,
  // subtitles and AI access.
  import { api } from '../../lib/api';
  import { app, act, loadStatus, toast } from '../../lib/state.svelte';
  import { bytes } from '../../lib/format';

  let general = $state<any>(null);
  let confirmActive = $state(false);

  // ---- AI access
  const mcpUrl = `${location.origin}/mcp`;
  function newKey() {
    return Array.from(crypto.getRandomValues(new Uint8Array(24)), (b) => b.toString(16).padStart(2, '0')).join('');
  }
  async function copyMcp(key: string) {
    const cmd = `claude mcp add --transport http --scope user spool ${mcpUrl} --header "Authorization: Bearer ${key}"`;
    try {
      await navigator.clipboard.writeText(cmd);
      toast('Copied. Paste it into a terminal.');
    } catch {
      prompt('Copy this command:', cmd);
    }
  }

  async function load() {
    general = await api.get('/settings/general');
  }
  load().catch(() => {});

  async function saveGeneral() {
    if (general.mode === 'active' && app.status?.mode !== 'active' && !confirmActive) {
      confirmActive = true;
      return;
    }
    confirmActive = false;
    await act(() => api.put('/settings/general', { ...general, recycle_days: Number(general.recycle_days), retention_days: Number(general.retention_days), rss_interval_minutes: Number(general.rss_interval_minutes), backlog_batch: Number(general.backlog_batch), backlog_interval_minutes: Number(general.backlog_interval_minutes), min_free_gb: Number(general.min_free_gb), post_parallel: Number(general.post_parallel), space_wait_gb: Number(general.space_wait_gb), archive_runner_ups: Number(general.archive_runner_ups), default_movie_profile: Number(general.default_movie_profile), default_series_profile: Number(general.default_series_profile), players_port: Number(general.players_port), new_titles_location: Number(general.new_titles_location) }), 'Saved');
    loadStatus();
    load();
  }
</script>

{#if general}
  <form class="stack form" onsubmit={(e) => { e.preventDefault(); saveGeneral(); }}>
    <fieldset class="card">
      <legend>Mode</legend>
      <label class="radio"><input type="radio" bind:group={general.mode} value="shadow" /><span><b>Shadow</b><small>Reads the feeds and records what it would do. Never downloads, moves or deletes.</small></span></label>
      <label class="radio"><input type="radio" bind:group={general.mode} value="active" /><span><b>Active</b><small>Downloads wanted releases and moves them into the library.</small></span></label>
      {#if confirmActive}
        <div class="confirm">
          <p><b>Make Spool active?</b> It will start downloading and writing to the library folders below. Stop Radarr and Sonarr from managing the same folders first, or two apps will fight over the same files.</p>
          <div class="row"><button class="btn primary" type="submit">Yes, make it active</button><button class="btn ghost" type="button" onclick={() => { confirmActive = false; general.mode = 'shadow'; }}>Stay in shadow</button></div>
        </div>
      {/if}
    </fieldset>

    <fieldset class="card">
      <legend>Downloads</legend>
      <label class="field">Downloads<input class="input mono" bind:value={general.downloads_dir} placeholder="/Volumes/Media/downloads" /><span class="hint">Keep this on the same disk as the library so finished files are moved, not copied.</span></label>
      <label class="field narrow">Keep free on the downloads disk (GB)<input class="input" type="number" min="0" bind:value={general.min_free_gb} /><span class="hint">Spool does not start a download that would leave less than this free once it has finished unpacking.</span></label>
      <label class="field narrow">Let downloads wait for room, up to (GB)<input class="input" type="number" min="0" bind:value={general.space_wait_gb} /><span class="hint">When the disk is short, Spool still queues what it finds, up to this much beyond the free space, and holds it without downloading until there is room. 0 makes it skip them instead.</span></label>
      <label class="check"><input type="checkbox" bind:checked={general.direct_unpack} /> Unpack while downloading</label>
      <span class="hint">Posts packed as uncompressed RAR volumes are unpacked as each volume arrives and the volume is deleted once verified, so a download needs about half the room and no extraction step. Anything else is handled the usual way.</span>
      <label class="field narrow">Downloads checked and unpacked at once<input class="input" type="number" min="1" max="8" bind:value={general.post_parallel} /><span class="hint">Takes effect the next time Spool starts.</span></label>
      <label class="field narrow">Keep replaced and deleted files for (days)<input class="input" type="number" min="0" bind:value={general.recycle_days} /><span class="hint">They wait in a <code>.spool-recycle</code> folder in the library and are removed automatically after this long. 0 deletes them at once, with no way back.</span></label>
    </fieldset>

    <fieldset class="card">
      <legend>Choosing releases</legend>
      <label class="field narrow">Fallback releases to save per download<input class="input" type="number" min="0" max="5" bind:value={general.archive_runner_ups} /><span class="hint">When Spool picks a release it also saves this many of the next-best ones to the <a class="link" href="#/archive">Archive</a>. Each costs one indexer download. 0 keeps only what is downloaded.</span></label>
      <label class="field narrow">Language<input class="input" bind:value={general.language} placeholder="english" /><span class="hint">Releases that name a different language are rejected, unless it is the language the film or series was made in. A foreign title is better with a track in it, and players start on that track. Leave empty to accept any.</span></label>
      <label class="field narrow">Propers and repacks
        <select class="input" bind:value={general.proper_policy}>
          <option value="prefer_and_upgrade">Prefer them, and replace an existing file</option>
          <option value="do_not_upgrade">Prefer them, but never replace a file for one</option>
          <option value="do_not_prefer">Treat like any other release</option>
        </select>
      </label>
      <label class="field narrow">Usenet retention (days)<input class="input" type="number" min="0" bind:value={general.retention_days} /><span class="hint">Older posts are rejected. 0 means no limit.</span></label>
      <div class="cols">
        <label class="field">Profile for new movies
          <select class="input" bind:value={general.default_movie_profile}>
            {#each app.profiles.filter((p) => p.kind === 'movie') as p}<option value={p.id}>{p.name}</option>{/each}
          </select>
        </label>
        <label class="field">Profile for new series
          <select class="input" bind:value={general.default_series_profile}>
            {#each app.profiles.filter((p) => p.kind === 'tv') as p}<option value={p.id}>{p.name}</option>{/each}
          </select>
        </label>
      </div>
      <div class="cols">
        <label class="field">Read feeds every (minutes)<input class="input" type="number" min="10" bind:value={general.rss_interval_minutes} /></label>
        <label class="field">Search missing every (minutes)<input class="input" type="number" min="60" bind:value={general.backlog_interval_minutes} /></label>
        <label class="field">Titles per missing search<input class="input" type="number" min="0" bind:value={general.backlog_batch} /></label>
      </div>
    </fieldset>

    <fieldset class="card">
      <legend>Connections</legend>
      <label class="field">TMDB API key<input class="input mono" bind:value={general.tmdb_api_key} autocomplete="off" /><span class="hint">Needed to look up and add movies. Free from themoviedb.org.</span></label>
      <label class="check"><input type="checkbox" bind:checked={general.plex_on} /> Use Plex</label>
      <span class="hint">{general.plex_on ? 'Spool tells Plex when a folder changes, reads what Plex holds once an hour, and links each title to its page there.' : 'Off: nothing is asked of Plex and nothing from it is shown. Its address and token are kept for when you turn it back on.'}</span>
      <div class="cols">
        <label class="field">Plex address<input class="input mono" bind:value={general.plex_url} placeholder="http://127.0.0.1:32400" disabled={!general.plex_on} /></label>
        <label class="field">Plex token<input class="input mono" bind:value={general.plex_token} autocomplete="off" disabled={!general.plex_on} /></label>
      </div>
      {#if general.plex_token && general.plex_on}
        <label class="check"><input type="checkbox" bind:checked={general.plex_collections} /> Write Spool's collections into Plex</label>
        <span class="hint">Each title in a collection gets the collection's name as a collection tag in Plex, beside any it already has. Plex keeps collections per library, so one that holds films and series appears in both. Removing a tag in Plex later is respected.</span>
      {/if}
      <label class="field">Second backup folder<input class="input mono" bind:value={general.backup_dir} autocomplete="off" placeholder="None" /><span class="hint">The nightly database backup is also copied here. Choose a folder on a different disk from the one Spool's data is on.</span></label>
    </fieldset>
    <fieldset class="card">
      <legend>Subtitles</legend>
      <p class="muted">Spool keeps the subtitle files that come with a release, and can look for more on OpenSubtitles. Every subtitle file it fetches is checked against the speech in the film: one that is out by a fixed amount is retimed, and one that does not fit is thrown away and the next tried.</p>
      <label class="field narrow">Languages wanted<input class="input" bind:value={general.subtitle_languages} placeholder="en" autocomplete="off" /><span class="hint">Codes or names, separated by commas: <code>en</code>, or <code>en, es</code>. A file with that language inside it or beside it needs nothing more.</span></label>
      <label class="field">OpenSubtitles API key<input class="input mono" bind:value={general.opensubtitles_api_key} autocomplete="off" /><span class="hint">Free from opensubtitles.com, under API consumers in your profile.</span></label>
      <div class="cols">
        <label class="field">OpenSubtitles username<input class="input" bind:value={general.opensubtitles_username} autocomplete="off" /></label>
        <label class="field">OpenSubtitles password<input class="input" type="password" bind:value={general.opensubtitles_password} autocomplete="new-password" /></label>
      </div>
      <label class="check"><input type="checkbox" bind:checked={general.subtitles_auto} /> Look for missing subtitles automatically</label>
      <span class="hint">A few files every six hours, newest first, to stay inside the account's daily download allowance. Each file is tried again after a week.</span>
    </fieldset>

    <fieldset class="card">
      <legend>AI access</legend>
      <p class="muted">Spool speaks the Model Context Protocol at <code>{mcpUrl}</code>, so an AI session can look things up, add and remove titles, start and manage downloads, read the log and work with Plex. Deleting files always needs an explicit confirmation.</p>
      <div class="row wrap">
        <button class="btn" type="button" onclick={() => copyMcp(general.api_key)} disabled={!general.api_key}>Copy the Claude Code command (full access)</button>
        {#if general.mcp_read_key}<button class="btn ghost" type="button" onclick={() => copyMcp(general.mcp_read_key)}>Copy it with the read-only key</button>{/if}
      </div>
      <label class="field">Read-only key
        <div class="row"><input class="input mono grow" bind:value={general.mcp_read_key} autocomplete="off" placeholder="None" /><button class="btn" type="button" onclick={() => (general.mcp_read_key = newKey())}>Generate</button></div>
        <span class="hint">Optional. A session using this key can look but not change anything. Save after changing it; empty turns it off.</span>
      </label>
    </fieldset>
    <div><button class="btn primary" type="submit">Save</button></div>
  </form>
{/if}
