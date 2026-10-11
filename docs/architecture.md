# How Spool is put together

Spool is one process. It holds the catalog, talks to indexers, downloads from Usenet, repairs and unpacks, files the result into the library, and serves the web app and API.

## Crates

| Crate | Responsibility |
| --- | --- |
| `spool-core` | Pure logic with no I/O: release-name parsing, the quality model, quality profiles, the decision engine that accepts, rejects and ranks releases, title matching, and file naming. |
| `spool-nntp` | The Usenet engine: NZB parsing, NNTP over TLS with pooled and pipelined connections, yEnc decoding, resumable assembly, PAR2 verification and repair, extraction, and unpacking stored RAR sets while they download. |
| `spool` | The server: SQLite store, metadata, indexer client, search and grab, the import journal, scheduler, HTTP API, MCP endpoint and CLI. |
| `web` | A Svelte app built to static files and embedded in the server binary. |

## The path of a download

1. **Search.** A scheduled feed read, a backlog search or a person asks the indexers for releases. Series are searched by id first, then by name if wanted episodes are still uncovered.
2. **Judge.** Every release gets a recorded verdict: accepted, or rejected with reasons (wrong title, quality not wanted, not an upgrade, wrong language, too large, blocklisted and so on). The verdicts are kept and shown, so "why didn't it get this one" always has an answer.
3. **Choose.** Accepted releases are ranked by the title's profile. For series the aim is a season from as few sources as possible. Releases over the profile's size target are a last resort, and so are dubs.
4. **Check.** A sample of the release's articles is looked up on the Usenet servers before anything is downloaded. A post that has mostly expired is blocklisted and the choice is made again.
5. **Download.** Articles are fetched across providers by priority, with fallback for anything missing. Progress is checkpointed, so a restart resumes instead of starting over.
6. **Verify and unpack.** Stored RAR volumes are unpacked as they arrive and deleted once their checksum is verified. Anything else is verified, repaired from PAR2 recovery data if needed, and extracted at the end.
7. **Import.** The plan (what moves where, what it replaces) is written to a journal before any file moves. Unfinished entries are completed at startup. Replaced files go to a recycle folder for a set number of days.
8. **Afterwards.** Plex is told to rescan the folder. The NZB is kept in the archive so the release can be fetched again later without an indexer.

## When the sources disagree about a work's shape

TMDB lists some works as one film that TVmaze lists as a series, and such works are posted as their episodes. An episode's release name carries the film's whole title, so judged one at a time it looks like the film. Spool settles it with the cheapest evidence it has, in this order:

1. **The other source.** When a film is added, and once at a later refresh for films still without a file, TVmaze is asked for a show with the film's IMDb id. Failing an id, a show of exactly the same name that began in the same year and runs about as long in all counts.
2. **The results as a set.** If a search for the film brings back releases naming three or more different episodes of it, and more of those than of releases that could be the whole film, it is posted in episodes. No single release shows this; the run does.
3. **One download.** A file far shorter than the film, from a release named as an episode, marks the title when it is stopped at import.

The same measuring settles the neighbouring cases at import (`plan` in `import.rs`): a film split into parts is filed as all of its parts, and a part released alone brings its siblings, which share its name but for the number; a file that runs as long as several consecutive episodes together is filed as those episodes; and a film far too long, or one file standing for a whole season, waits for a person to say yes. Import anyway is that yes, and is not asked for twice.

A film so marked turns down releases named as its episodes (`episode` in the verdicts) and still accepts the film whole. `film_to_series` replaces it with the series the other source lists, hands over any download that was stopped, and files it as the episode its name says it is.

## Anime and collections

`anime.rs` adds a third source for titles that are anime. A community list ties AniList's ids to IMDb's and TVDB's, so a title is found by an id it already has. AniList then gives the work's format, its episode count, the names releases carry, and its relations to other works; one request covers ten titles and reaches two works out from each. What it says is kept in the `anime` table.

Collections are worked out when asked for (`anime::groups`), from the titles and that table: works joined by a story relation are one story, and a story with two or more titles in the library is a collection, as is a TMDB film collection with two or more films here. The web app, the players' server and the Plex task all read the same result.

## Storage

One SQLite database in WAL mode holds everything, in a "columns plus JSON document" shape: the columns that are queried are real columns and the rest of each record is JSON. Saved NZBs are gzip files beside it. The database contains indexer keys and Usenet passwords and is created readable only by its owner.

## Locations

The library always has a main location: the two folders in the general settings, on the volume named there. Further locations sit beside it, each with folders of its own and each either managed or only read. A title belongs to the location whose folder holds its own, so nothing about a title says where it is except its path.

Two rules follow from that, and every part of Spool that touches files asks the same question of them (`title_locked`):

- **Away means left alone.** A location that cannot be reached takes its titles offline. They are not scanned, searched for or offered to players, and a file that cannot be seen is never recorded as gone. Whether a location is there is looked at on a thread of its own with a short wait, because a network share that has gone can keep a caller waiting indefinitely.
- **Only read means nothing is written.** On a location Spool does not manage, releases are rejected with the reason, imports and subtitle files are refused, and nothing is deleted or recycled.

Bringing in what is already on a location (`adopt.rs`) is two steps: looking reads each folder's name, asks the metadata sources and reports what it is sure of, what is already in the library, what is unclear and what it could not place; adding records the sure ones, and any the user settles, where they lie.

## What else is in a folder

A title's folder usually holds more than the film: extras, artwork, theme music and the notes of whichever media server looked after it before. `local.rs` says what each file is from where it lies and what it is called, following the conventions of Plex, Jellyfin, Emby and Kodi ([local-files.md](local-files.md) lists them). What it finds is kept on the title as `local` and refreshed when the folder is scanned, after each import and once a night.

Only a video that is the film, a version or part of it, or an episode becomes a library file. That is what keeps an upgrade from sweeping away a featurette: the import replaces the film's main file and nothing else. Where a film folder holds several films' worth of video, the largest is the film and the rest are versions kept beside it, unless they are the numbered parts of one film. When an upgrade gives the film a new name, the artwork and notes a media server had named after the old file are renamed to follow it; nothing else in the folder is touched.

## Knowing what changed

The database keeps a count of changes to the library. Triggers move it on by one for every write to a title, episode or file that alters it, and note in a `changes` table where that row now stands: one entry per row, holding its latest position, with removals kept for three months. Because the database does this itself, no part of Spool can change the library without it being recorded. Saving a row as it already was does not count, and neither does a title whose only news is when it was last searched or refreshed.

Anything that keeps a copy of the library asks for what moved after the count it last saw:

- The web app's title list (`GET /api/titles?since=N`) receives only the titles that changed and the ids of those that went.
- The players' server rebuilds its picture of the library only when the count has moved.
- The Plex task asks Plex to rescan the folders whose files changed since its last reading.
- `GET /api/changes?since=N` gives the same record to other programs.

A caller whose count is older than the oldest removal still remembered, or is one this database never gave out, is told to read everything again.

## Safety rules the code enforces

- If the required volume is not mounted or not readable, nothing is downloaded, imported or created. Spool never makes stand-in folders on another disk.
- Spool starts in shadow mode: it records what it would do and touches nothing until switched to active.
- Downloads that the disk cannot hold wait in the queue instead of starting.
- A release that fails is blocklisted and the next acceptable one is tried.
- Deleting files through the MCP endpoint needs an explicit confirmation.

## Where the parsers come from

Release-name parsing is ported from [Radarr](https://github.com/Radarr/Radarr) and [Sonarr](https://github.com/Sonarr/Sonarr), both GPLv3. Sonarr's title patterns are transpiled by `tools/gen_sonarr_regexes.py`, and both projects' parser test cases are extracted by `tools/extract_fixtures.py` and run as Spool's fixtures (`crates/spool-core/tests/upstream_fixtures.rs`).
