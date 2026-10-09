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

## Storage

One SQLite database in WAL mode holds everything, in a "columns plus JSON document" shape: the columns that are queried are real columns and the rest of each record is JSON. Saved NZBs are gzip files beside it. The database contains indexer keys and Usenet passwords and is created readable only by its owner.

## Safety rules the code enforces

- If the required volume is not mounted or not readable, nothing is downloaded, imported or created. Spool never makes stand-in folders on another disk.
- Spool starts in shadow mode: it records what it would do and touches nothing until switched to active.
- Downloads that the disk cannot hold wait in the queue instead of starting.
- A release that fails is blocklisted and the next acceptable one is tried.
- Deleting files through the MCP endpoint needs an explicit confirmation.

## Where the parsers come from

Release-name parsing is ported from [Radarr](https://github.com/Radarr/Radarr) and [Sonarr](https://github.com/Sonarr/Sonarr), both GPLv3. Sonarr's title patterns are transpiled by `tools/gen_sonarr_regexes.py`, and both projects' parser test cases are extracted by `tools/extract_fixtures.py` and run as Spool's fixtures (`crates/spool-core/tests/upstream_fixtures.rs`).
