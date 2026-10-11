# AI assistants

Spool serves the [Model Context Protocol](https://modelcontextprotocol.io) at `/mcp`, so an assistant such as Claude can look at the library, explain what happened to a title, and operate Spool.

## Connecting

The endpoint always requires a key, whether or not the web app has a password. Spool makes a key when it first starts. Settings, General, AI access has a button that copies the command to add Spool to Claude Code:

```sh
claude mcp add --transport http --scope user spool http://localhost:7979/mcp --header "Authorization: Bearer <key>"
```

Any client that speaks the protocol over HTTP can connect the same way, sending the key as `Authorization: Bearer <key>`.

### A key that can only look

The same settings can make a second, read-only key. A session using it can use the tools that look things up and none that change anything. Leave the field empty to turn it off.

## What an assistant can do

| Area | Tools |
| --- | --- |
| The overall picture | `status`, `activity`, `logs`, `disk_space`, `restart_check` |
| Titles | `find_title`, `title_detail`, `discover`, `add_title`, `update_title`, `refresh_title`, `remove_title`, `bulk_titles`, `film_to_series` |
| Locations | `helpers`, `locations`, `check_location`, `look_through_location`, `add_from_location`, `move_titles`, `cancel_moves` |
| Collections | `collections`, `add_missing_work` |
| Profiles | `profile_fit` |
| Compact versions | `compact_versions`, `make_compact_versions`, `cancel_compact_versions` |
| Why something has not arrived | `diagnose` |
| Releases and downloads | `search_releases`, `download_release`, `manage_download`, `unblock_release`, `dismiss_attention` |
| The archive | `archive`, `check_availability` |
| Subtitles | `subtitles`, `find_subtitles`, `check_subtitles` |
| Plex | `plex_report`, `plex_sync`, `plex_track` |
| Housekeeping | `run_task`, `empty_recycle` |
| Deleting files | `delete_file`, `delete_title_files` |

`restart_check` says whether Spool can be restarted without interrupting a download being unpacked, an import, a move or someone playing; an assistant should ask it before restarting the service.

`diagnose` answers "why has this not downloaded" in one call: whether the title is monitored, the releases that were rejected and why, downloads that failed, and the log lines that mention it.

## Deleting needs a yes

Any tool that deletes files refuses unless it is called with an explicit confirmation, and the tools tell the assistant to use it only when you asked for that deletion. Deleted files go to the [recycle folder](your-library.md#getting-something-back) like any others.

## Things to ask

- "Why hasn't the new season arrived?"
- "What is using the most disk space that I have already watched?"
- "Add this film and tell me which release you would pick."
- "Something failed last night. What happened?"
