# The HTTP API

The web app talks to Spool over a JSON API under `/api`, and other programs can use it too. It is the app's own API: it is not versioned, and it changes when the app does. The parts described here are the ones meant for other programs.

For an AI assistant, use the [MCP endpoint](assistants.md) instead.

## Signing in

If the web app has no password, the API is open to anyone who can reach the address.

If it has one, send Spool's key with each request:

```sh
curl -H "X-Api-Key: <key>" http://localhost:7979/api/status
```

The key is the one in the command that Settings, General, AI access copies.

## Status

`GET /api/status` gives the mode, the version, how many movies, series and files there are, free space, whether the queue is paused, the scheduled tasks and any warnings, such as a location that cannot be reached.

## Before restarting

`GET /api/busy` says whether stopping Spool now would interrupt something that does not simply carry on afterwards:

```json
{"busy": true, "reasons": ["1 download is being checked, repaired or unpacked"], "resumable": 2}
```

A download still coming down is not a reason: it is checkpointed and resumes, and is counted under `resumable`. `deploy/deploy.sh` asks this and stops if the answer is yes.

## Asking what changed

Spool keeps a count of changes to the library. Every write that alters a title, an episode, a file or what has been watched moves it on by one. A program that keeps its own copy of the library remembers the count it last saw and asks for what moved after it.

### Titles

`GET /api/titles` gives every title. `GET /api/titles?since=N` gives only what changed after count `N`:

```json
{
  "revision": 5120,
  "full": false,
  "titles": [],
  "removed": [812]
}
```

| Field | Meaning |
| --- | --- |
| `revision` | The count now. Send it as `since` next time. |
| `full` | True when `titles` is the whole library, because `N` was too old or not one this database gave out. Replace your copy instead of merging. |
| `titles` | Titles that are new or changed. |
| `removed` | Ids of titles that have gone. |

### Everything else

`GET /api/changes?since=N` gives the record itself, oldest first.

| Parameter | Meaning |
| --- | --- |
| `since` | The count you last saw. 0 starts from the beginning. |
| `kinds` | Optional, separated by commas: `title`, `episode`, `file`, `activity`, `watched_movie`, `watched_series`, `watched_episode`. |
| `limit` | How many to return, 500 unless given, 5000 at most. |

```json
{
  "revision": 5120,
  "floor": 0,
  "complete": true,
  "more": false,
  "changes": [
    {"kind": "file", "id": 311, "title_id": 42, "rev": 5119, "removed": false, "at": 1791590400}
  ]
}
```

There is one entry per row, holding where it stands now, not one per write. `removed` marks something that has gone; removals are remembered for 90 days. `complete` is false when `since` is older than the oldest removal still remembered, in which case read everything again. `more` means the limit was reached: ask again from the last `rev` you received.

[How Spool is put together](architecture.md#knowing-what-changed) explains how the count is kept.

## Following events

`GET /api/events` is a stream of server-sent events, one JSON object each, sent as things happen: a title changed, a download progressed, a location came or went. A client that falls behind receives `{"type":"resync"}` and should read its state again.

## Doing things

The routes the web app uses are open to other programs as well. A few that are useful from a script:

| Request | Effect |
| --- | --- |
| `POST /api/queue/pause`, `POST /api/queue/resume` | Stop and start downloading. |
| `POST /api/tasks/{name}` | Run a scheduled task now: `rss`, `backlog`, `refresh`, `scan`, `housekeeping`, `plex` or `subtitles`. |
| `POST /api/titles/{id}/search` | Search for a title now. |
| `POST /api/titles/{id}/scan` | Rescan a title's folder. |
| `POST /api/titles/move` | Move titles to another location: `{"ids": [...], "location": id}`. |
| `POST /api/titles/{id}/to-series` | Replace a film that is posted in episodes with the series it is listed as. |
| `GET /api/lookup/all?q=…` | Films and series by name, together, with what AniList says of any that are anime. |
| `GET /api/collections` | The library's collections, each with its members, their roles and order, and what is missing. |
| `GET /api/locations` | Each location, whether it can be reached, and what is on it. |
