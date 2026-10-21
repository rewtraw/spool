# The HTTP API

The web app talks to Spool over a JSON API under `/api`, and other programs can use it too. It is the app's own API: it is not versioned, and it changes when the app does. The parts described here are the ones meant for other programs.

For an AI assistant, use the [MCP endpoint](assistants.md) instead.

## Signing in

While nobody under Settings, Players, People has a password, the API is open to anyone who can reach the address, and Status warns of it.

Once anyone has one, send Spool's key with each request:

```sh
curl -H "X-Api-Key: <key>" http://localhost:7979/api/status
```

The key is the one in the command that Settings, General, AI access copies. A program with the key may do anything, and is nobody in particular, so it has nothing watched of its own.

The web app signs in as a person: `POST /api/login` with `{"name": ..., "password": ...}` sets a cookie for that browser, and `POST /api/logout` clears it. `GET /api/session` says whether sign-in is asked for (`required`), who is signed in (`me`) and whether they run Spool (`manages`). While the app is open, it signs the browser in as the first person who runs Spool, so the browser stays theirs once passwords are given.

Someone who does not run Spool may read the library (`GET` of `/api/titles`, a title, `/api/collections` and artwork), keep what they have watched, and sign out. Anything else gets `403`. A title's page leaves out, for them, the releases weighed, what was downloaded and from where.

Five wrong passwords in a row from one address shut sign-in to that address for a minute. Other addresses can still sign in, and a right password doesn't make up for the wrong guesses before it.

Two rules keep a web page you happen to open from using Spool through your browser:

- **Spool answers only to names your network gives it:** an IP address, a name without a dot such as `casper`, or a name ending in `.local`, `.ts.net`, `.lan`, `.home.arpa`, `.internal` or `.localhost`. Any other name gets `421`, even for a read. To put Spool behind a domain of your own, have the proxy pass on one of those names as the `Host`.
- **A change sent from a browser must come from Spool's own pages.** A browser marks every request with the page it came from (`Origin`). A change from another page gets `403`, even another port on the same machine. Programs such as `curl` send no `Origin` and aren't affected.

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

Compact versions: `GET /api/compact` says how the library stands for them and what is being made; `POST /api/compact` with `{"title_ids": [...]}` or `{"file_ids": [...]}` puts files in line and names those refused; `POST /api/compact/everything` puts in line all that would gain from one, or with `{"count": n}` the biggest `n`; `POST /api/compact/cancel` with `{"file_ids": [...]}` calls them off, or all that wait when none are named; `POST /api/compact/hold` with `{"hold": true}` begins no more and lets those being made finish, until it is sent `false` or Spool restarts; `POST /api/files/{id}/keep-compact` lets the original go. A file that is a compact version carries `compact_of`, the id of the file it was made from, and `GET /api/activity` lists those being made and those waiting under `compact`: `now` is a list, one for each machine at work, and each names the linked machine making it in `by`, which is empty for this one.

The Spools this one is linked to, such as a helper on a NAS, are at `GET /api/links`: what each serves, what it allows (`see`, `play`, `work`), whether it hands films to players itself (`play`) and looks through its own folders (`see`), and how it was last found. What players are playing now, and the machine each takes it from, is under `playing` in `GET /api/activity` and `GET /api/players`. A download that runs on one has an id beginning `@`, and its entry in `GET /api/activity` names the helper under `on`.

Clips: `POST /api/clips` with `{"title_id", "start", "seconds"}`, and for a series `episode_id` or `season` and `episode`, asks for one and answers with it at once, `state` `making`; `start` and `seconds` are seconds or written as `30m`, `1h2m5s` or `1:02:05`, and an optional `name` names it. `GET /api/clips` lists them newest first, each with its `state` (`making`, `ready` or `failed`, with `error`), `progress` while it is made, and `by`, who made it. `GET /api/clips/{id}/video` is the MP4, in parts as a player asks, and with `?download` named to be saved; `GET /api/clips/{id}/still` is a picture from it; `DELETE /api/clips/{id}` removes it. `GET /clip/{link}.mp4`, with the clip's `link`, is the clip without signing in. A `clip` event is sent when one is asked for, made, fails or is removed.

The people who play are at `GET /api/accounts`: each account with the players signed in to it, what it is playing and what it has played in all, and the latest sittings. `POST /api/accounts` adds one and `PUT /api/accounts/{id}` changes its `name`, `password`, `enabled`, `manages` (whether the person runs Spool), `adds` (whether what it asks for is added at once) or `compact_away` (whether, away from home, it is given compact versions only). A change or a removal that would leave nobody who runs Spool gets `409`. A new password signs the person out of every player and browser but the one that set it. `POST /api/accounts/{id}/plex` brings what was watched in Plex to it. What has been asked for is at `GET /api/asks`, each with who asked and how it stands (`asked`, `coming`, `ready`, `declined`, `gone`); `POST /api/asks/{id}/add` and `/decline` answer one, and `DELETE /api/asks/{id}` clears it away. How many wait for an answer is `asks` in `GET /api/status`.

What the person signed in has watched comes with the library: each title in `GET /api/titles` has `watched` when they have started it, with `done` and `part` (how much of a film is seen, or how many of a series' episodes). A title's page has `watched` with the place they stopped (`position` and `runtime`, in seconds) and, for a series, `watched_episodes` by episode. `POST /api/watched` with `{"title_id": ..., "played": true}` marks a film or a whole series watched, or not with `false`; add `season` for one season or `episode_id` for one episode. `GET /api/watching` is what they were part-way through and the next episode of what they watch, the most recent first. It is the same record players keep, so a mark made in either shows in the other.

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
| `POST /api/titles/move` | Move titles to another location: `{"ids": [...], "location": id}`. The answer says how many were put in line and names those refused, each with the reason. |
| `POST /api/titles/move/cancel` | Call moves off: `{"ids": [...]}` takes those titles out of the line and stops the move under way if its title is among them; with no ids, every waiting title is taken out. `GET /api/activity` lists the move under way and those waiting, under `moves`. |
| `POST /api/titles/{id}/to-series` | Replace a film that is posted in episodes with the series it is listed as. |
| `GET /api/lookup/all?q=…` | Films and series by name, together, with what AniList says of any that are anime. |
| `GET /api/collections` | The library's collections, each with its members, their roles and order, and what is missing. |
| `GET /api/locations` | Each location, whether it can be reached, and what is on it. |
