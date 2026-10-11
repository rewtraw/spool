# Your library and what Spool does to it

This page is for anyone pointing Spool at films and shows they already have. It says how to bring a library in, exactly what Spool changes on disk, what it never touches, and how to get something back.

## The short answer

- In shadow mode, and on a location marked as only read, Spool changes nothing on disk.
- On a location it manages, in active mode, Spool adds files, replaces a film or episode with a better copy when the title's profile allows it, and deletes what you tell it to. Replaced and deleted files wait in a recycle folder for seven days.
- It never moves or renames the files already in your library, and never touches trailers, extras, artwork or a media server's notes, with one small exception described [below](#one-exception-files-named-after-the-film).

The real risks are an unwanted upgrade and a folder matched to the wrong film. Both are avoided by bringing titles in unmonitored and looking before you switch things on.

## Bringing in a library you already have

There are three ways in. None of them moves, renames or deletes anything.

| You have | Use | What it does |
| --- | --- | --- |
| Radarr and Sonarr | `spool migrate` | Copies their catalog, profiles, indexers and Usenet servers. Titles keep the monitoring they had. See [Getting started](getting-started.md#coming-from-radarr-sonarr-and-sabnzbd). |
| Plex | Settings, Plex, **Track in Spool** | Adds a title Plex has, unmonitored, pointing at the folder Plex plays it from. See [Beside Plex](plex.md). |
| Folders and nothing else | Settings, General, More locations, **Look for films and shows** | Reads each folder's name and any notes inside it, and offers what it recognised for you to add. See [A library in more than one place](locations.md). |

The third way works on further locations, not on the main folders. If your only library is a set of plain folders, give Spool an empty main folder for new titles and add the existing library as a location.

## What Spool changes, and when

Everything in this table needs active mode and a location Spool manages.

| What | When | Where the old file goes |
| --- | --- | --- |
| Creates a title's folder and files a download into it | A monitored title is missing and an acceptable release is found | Nothing is replaced |
| Renames the downloaded file to your naming format | On import, if Naming has "Rename files when importing" on | Nothing is replaced |
| Replaces a film or an episode with a better copy | The title is monitored, its profile allows upgrades, and the file is below the profile's cutoff | Recycle folder |
| Replaces a file with a smaller copy | You ask for it, with Find smaller copies | Recycle folder |
| Writes subtitle files beside a film | The release brought them, or you set languages under Subtitles | Nothing is replaced |
| Deletes a file | You ask for it: Delete files, or removing a title and choosing to delete its files | Recycle folder |

A title that is not monitored is never searched for, downloaded or upgraded. Removing a title from Spool leaves its files where they are unless you ask otherwise.

## What Spool never does

- **Move or rename files already in the library.** `spool check-paths`, and the same check under Settings, Maintenance, only report where files differ from the naming format.
- **Touch what else is in the folder.** Trailers, featurettes, samples, posters, backdrops, theme music, `.nfo` files and `.plexmatch` are recognised and left alone. An upgrade replaces the film's main file and nothing else. Second versions of a film are left alone too. [Files Spool recognises](local-files.md) lists every name.
- **Write to a location marked as only read.** No downloads, upgrades, renames, subtitles or deletions happen there.
- **Write when the disk is missing.** If the disk named in the settings is not mounted, Spool pauses. It does not create stand-in folders somewhere else.
- **Take a library that has gone away for one that needs downloading again.** Titles on a location that cannot be reached are shown as offline and left alone.

### One exception: files named after the film

Plex, Jellyfin and Kodi attach artwork and notes to a film by name: `Film (1999)-fanart.jpg` and `Film (1999).nfo` belong to `Film (1999).mkv`. When an upgrade gives the film's file a new name, Spool renames those companions to match, so the media server still finds them. It never overwrites a file to do so, and does it only on locations it manages.

## What can go wrong

**An upgrade you did not want.** If a title is monitored and its profile allows upgrades, Spool replaces a file it judges worse than the cutoff. A library you are happy with should come in unmonitored, or on a location that is only read. Monitor titles one at a time, or change the profile so that upgrades are off, before monitoring many.

**A folder matched to the wrong film.** Two films share a name, or a folder is named carelessly. When looking through a location, Spool adds only what it is sure of, lists the rest as unclear for you to choose, and notes when a file runs much longer or shorter than the film it was matched to. A wrong match changes nothing on disk: the title shows the wrong details until you remove it and add the right one. It matters more if the title is monitored, because Spool would then look for the wrong film.

**A file that disappears while its title is monitored.** The library scan notices files added and removed outside Spool. A monitored title whose file has gone from a disk that is there counts as missing, and Spool looks for it again. Stop monitoring a title before deleting its file by hand, or delete it through Spool, which does that for you.

**Two programs managing one folder.** Stop Radarr and Sonarr before making Spool active. Plex and Jellyfin only read, and can stay.

**An extra taken for the film.** Spool tells extras from the film by the folder and file names media servers use. A featurette with an unmarked name, loose in a film's folder, looks like a second version of the film. It is kept and never replaced, and it is listed on the title's page as a version, where you can see it.

## Getting something back

- **The recycle folder.** Replaced and deleted files are moved to a `.spool-recycle` folder in the library they came from, and removed after the number of days set under Settings, General, Folders (seven unless you change it). Setting it to 0 deletes at once, with no way back.
- **The import journal.** What an import will move and replace is written down before any file moves. An import interrupted by a restart or a power cut is finished at the next start.
- **The archive.** Spool keeps the NZB of everything it downloads, so a file deleted to make room can be downloaded again while the post is still on Usenet.
- **Backups.** The database is backed up each night into Spool's data folder, and to a second folder if you name one.
- **History.** Every import, replacement and deletion is listed under Activity and on the title's page, with the reason.

## A careful way in

1. Install Spool and leave it in shadow mode.
2. Bring the library in by one of the three ways above, unmonitored, or as a location that is only read.
3. Open a few titles. Check the details are right and that the page lists the files you expect, with any extras under "Also in the folder".
4. Run the check under Settings, Maintenance, File locations to see how your naming compares with Spool's.
5. Add a title you do not have, monitored, and read the verdicts Spool gives its releases.
6. Switch to active. Monitor more as you come to trust it.
