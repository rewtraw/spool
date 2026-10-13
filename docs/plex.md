# Beside Plex

Spool can [serve a television by itself](players.md), and it also works beside a Plex server you already run. The two can be used together. Plex only reads the library, so it and Spool never compete over files.

## Connecting

Under Settings, General, Connections, enter Plex's address, such as `http://127.0.0.1:32400`, and a Plex token. `spool migrate` brings the token over from Radarr if it had one.

## Turning it off

**Use Plex**, under Settings, General, turns all of this off without forgetting the address and token. Off, Spool asks nothing of Plex and shows nothing from it: no refresh when a folder changes, no hourly read, no Open in Plex links, no Plex tab. Turn it back on and it carries on from where it was. Plex itself is not touched either way.

## What Spool does with Plex

- **Matches titles.** Once an hour Spool reads Plex's films and shows and matches them to its own titles. A matched title has an Open in Plex link on its page.
- **Asks for a rescan after an import.** When files change, Spool asks Plex to rescan the folders concerned, not the whole library, so a new film appears in Plex without waiting for its own schedule.
- **Knows what has been watched.** Settings, Disk space shows, for the largest titles, whether Plex says they were watched and when. That is what makes "delete what I have watched" a safe thing to ask for.

Spool changes nothing in Plex beyond asking for a rescan, unless you turn on **Write Spool's collections into Plex** under the same settings. It then adds a collection tag to each title that is part of one of [Spool's collections](collections.md), beside the tags the title already has.

## The Plex tab

Settings, Plex compares the two libraries.

| List | Meaning | What you can do |
| --- | --- | --- |
| In Plex, not in Spool | Plex has a film or show that Spool does not track. | **Track in Spool** adds it unmonitored, pointing at the folder Plex plays it from. Nothing is moved, renamed or downloaded. |
| In Spool with files, not in Plex | Plex may not have scanned it yet, may have matched it to the wrong film, or the files may be gone. | **Rescan folder** checks the folder on disk. |

Track in Spool is the gentlest way to bring a library that Plex already knows into Spool. See [Your library and what Spool does to it](your-library.md).

## What Plex leaves in a folder

Plex, like Jellyfin and Kodi, reads trailers, extras and artwork from a title's folder by name, and can be told what a folder holds with a `.plexmatch` file. Spool recognises the same names, leaves those files alone, and keeps files named after the film in step when an upgrade renames it. [Files Spool recognises](local-files.md) has the full list.
