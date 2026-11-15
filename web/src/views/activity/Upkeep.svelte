<script lang="ts">
  // Upkeep: the work Spool does by itself, each with a button to do it now, and how the
  // library stands beside Plex, the music folders and the book index.
  import { api } from '../../lib/api';
  import { app, act } from '../../lib/state.svelte';
  import { ago } from '../../lib/format';
  import PlexMatch from './PlexMatch.svelte';
  import MusicFolder from './MusicFolder.svelte';
  import BookIndex from './BookIndex.svelte';
  import ProfileFit from './ProfileFit.svelte';

  const TASKS = [
    ['rss', 'Read indexer feeds', 'Checks every indexer for new releases the library wants, every 15 minutes.'],
    ['backlog', 'Search for missing titles', 'Searches a few missing titles, least recently searched first, every six hours.'],
    ['refresh', 'Refresh details', 'Updates titles and episode lists from TMDB and TVmaze.'],
    ['scan', 'Rescan library folders', 'Finds files added or removed outside Spool. Changes the catalog only.'],
    ['housekeeping', 'Back up and tidy', 'Writes a database backup and empties recycle folders older than a week.'],
    ['plex', 'Read Plex', "Matches Plex's films and shows to titles in Spool."],
    ['anime', 'Read AniList', 'Finds which titles are anime and reads their format, names and relations from AniList.'],
    ['subtitles', 'Find missing subtitles', 'Looks for subtitles in the languages chosen for a few files that lack them, keeping only ones in time with the film.'],
    ['music', 'Read the music folders', 'Finds albums added or removed in the music folders.'],
    ['music_wants', 'Get wanted albums', 'Looks for each wanted album and downloads the best release.'],
    ['books', 'Get wanted books', "Looks for each wanted book in the book index, downloads it from Anna's Archive and sends it to Florilegium."],
    ['book_index', 'Update the book index', "Builds the book index again when Anna's Archive has published a newer snapshot. Reads about 170 GB."],
    ['videos', 'Look at followed channels', 'Fetches what followed channels have posted since they were last looked at.'],
    ['lists', 'Read followed lists', 'Adds the films put on followed Letterboxd lists since they were last read.'],
  ];
  async function runTask(name: string) {
    await act(() => api.post(`/tasks/${name}`), 'Started');
  }
</script>

  <div class="stack form">
    <div class="card list">
      {#each TASKS as [name, label, hint]}
        {@const s = app.status?.tasks?.[name]}
        <div class="task">
          <div class="grow"><b>{label}</b><div class="faint small">{hint}</div>{#if s?.last_message}<div class="muted small">{s.running ? 'Running…' : `${ago(s.last_run)}: ${s.last_message}`}</div>{/if}</div>
          <button class="btn small" disabled={s?.running} onclick={() => runTask(name)}>{s?.running ? 'Running…' : 'Run now'}</button>
        </div>
      {/each}
    </div>
    <PlexMatch />
    <MusicFolder />
    <BookIndex />
    <ProfileFit />
    <p class="faint small">Spool {app.status?.version}. Movie details and artwork come from TMDB; series details come from TVmaze, or from TMDB for a series TVmaze does not list; anime formats and relations come from AniList. This product uses the TMDB API but is not endorsed or certified by TMDB.</p>
  </div>
