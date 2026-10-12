# A library in more than one place

Spool always has a main location: the first one listed under This machine in Settings, General, Library. Beside it you can add further locations, each with folders of its own: another disk, or a share from a NAS mounted on the machine Spool runs on. The library shows them as one.

This part of Spool is new. It is tested with folders that come and go; it has had little use yet with a network share over weeks.

## Managed, or only read

Each location has a box, "Let Spool change files here".

| | Only read (box off) | Managed (box on) |
| --- | --- | --- |
| Listed in the library and offered to players | Yes | Yes |
| Posters and details | Yes | Yes |
| Searching, downloading, upgrading | No | Yes |
| Subtitle files written | No | Yes |
| Files deleted or recycled | No | Yes |
| Can receive new titles | No | Yes |

Only read is the right choice for a library that something else looks after, or one you want kept exactly as it is. Spool lists and plays what is there and writes nothing.

## Adding a location

1. **Mount it first.** Spool reads ordinary folders. A share from a NAS has to be mounted by the system, at the same path every time, before Spool can use it. See [Mounting a share](#mounting-a-share).
2. Under Settings, General, Library, choose **Add a location** and fill in:

   | Field | What to put |
   | --- | --- |
   | Name | What to call it, such as the NAS's name. |
   | Movies, Series | The folders that hold them. Either may be left empty. |
   | Disk or share that must be mounted | The mount point, such as `/Volumes/Goliath`. Leave it empty if the folders only have to be readable. |
   | Let Spool change files here | Off for only read. |

3. **Save.** The location now shows whether it can be reached, how many titles are on it and how much room is free.
4. **Check this location.** The button tries the location out: whether each folder is there, can be listed, can be read and how fast, and, where Spool is to change files, can be written (a small test file is written and removed). Anything that fails comes with its likely cause.

Each location can have its own **profile for films and for series**, given to titles added there or brought in from there. A big disk kept as an archive and a small one for everyday viewing want different things: under a profile with a size target, a shelf of untouched Blu-ray copies would all count as too big. Settings, Profiles shows how each profile fits what is on disk.

In a container, the location's folders must be mounted into the container as well, and the paths you enter are the ones the container sees.

## Bringing in what is already there

**Look for films and shows** reads the location and reports before it adds anything. Looking changes nothing, and neither does adding: titles are recorded where they lie.

Each folder directly inside Movies or Series is taken as one title. Spool works out which from:

- the folder's name, as `Title (Year)`;
- an id in the name, written as Plex does (`{tmdb-603}`) or as Jellyfin and Emby do (`[tmdbid-603]`);
- the notes a media server left inside: `movie.nfo`, `tvshow.nfo` or `.plexmatch`.

The result comes in groups:

| Group | Meaning |
| --- | --- |
| To add | Spool is sure what it is. |
| Unclear | More than one film or show fits. Choose one, or leave it out. |
| Already in the library | The title is already recorded with a file, here or on another location. It is skipped. |
| Not recognised | Nothing matched. Renaming the folder to `Title (Year)` and looking again usually settles it. |

A folder may turn out to hold a title that is already in the library. It is still listed under To add, and adding does not make a second entry for it:

- **The title has no file yet.** This is what a library looks like after coming over from Radarr and Sonarr: the catalogue came across, and the files are where they always were. The title is pointed at the folder that holds its file, and keeps its monitoring and profile.
- **The title has its files somewhere else.** The folder is kept as another copy of it. See [One title in two places](#one-title-in-two-places).

Then **Add to the library**. On a managed location you can choose to monitor what is added, so that Spool looks for upgrades and missing episodes; leave that off for a library you want left as it is. A location that is only read does not offer the choice, since nothing there is ever searched for.

After adding, a title whose file runs much longer or shorter than the film it was matched to is noted, so a wrong match stands out.

## One title in two places

A title has one folder of its own, where Spool files what it downloads for it. It may also be held in folders on other locations:

- **A film** kept in two places has both copies listed on its page, and players are offered both as versions, the copy named for the place it is on.
- **A series** takes from the other folder the episodes its own folder lacks, so its seasons can be split between two places. An episode held in both is played from the title's own folder.

Spool plays a copy and never changes it, whatever kind of location it is on: it is not replaced by an upgrade, renamed, given subtitles or deleted, and removing the title's files leaves it where it is. If one place is away, the title stays playable from the other.

## Moving a title to another location

**Move to…** on a title's page, or in the bar that appears when titles are selected in the library, takes a title's folder to another location Spool manages. Titles are moved one at a time, and a title stays playable where it is until its move is done.

1. The whole folder is copied to the other location under a working name: the film or episodes, subtitles, extras, artwork and notes, a media server's hidden notes such as `.plexmatch` among them. What a file system keeps for itself (`.DS_Store`, `._` files, `.AppleDouble`, `@eaDir`) is not copied, and is cleared away with the original.
2. Every file is checked against its original.
3. The copy is given the folder's name and the title is pointed at it.
4. Only then is the original taken away.

Activity shows the title being moved, how much of it has been copied and how fast, and the titles waiting their turn. 

A move can be cancelled there or on the title's own page. A waiting title is simply taken out of the line. Cancelling the move under way stops the copy and removes what was copied so far; the title stays where it was, untouched. Once the copy is complete the move can no longer be cancelled and finishes.

If anything fails before the last step, the title is left where it was and a notice says why. On one disk the folder is simply renamed into place.

A location Spool only reads gives nothing up. Moving a title off one copies it, and the original stays there as [a further copy](#one-title-in-two-places).

A move is refused, with the reason, when the other location is away, is only read, has no room, or already has a folder of that name; and while the title has a download under way. Room is reckoned for the whole line: a title is refused when you ask if it would not fit once the titles ahead of it have arrived, so a batch does not run out of room halfway.

The line of waiting titles is written down. If Spool is restarted, it clears away the half-made copy of the move that was interrupted and takes the line up again from that title.

## When a location cannot be reached

A NAS that is switched off, or a disk that is unplugged, takes its titles offline:

- They stay in the library, marked **Offline**.
- They are not searched for, downloaded again or upgraded.
- Files that cannot be seen are not recorded as gone.
- Players are not offered them, so nothing fails to play.
- Status shows a notice naming the location.

When the location comes back, its titles come back with it. Nothing needs doing.

Spool checks each location on a thread of its own with a short wait, because a network share that has gone can otherwise keep a program waiting for minutes.

## Where new titles go

Once there is a managed location beside the main one, "Put new titles on" chooses which receives titles you add, and the box for adding a title under Discover offers the same choice for that one title. Titles already in the library stay where they are. If the chosen location cannot be reached when a title is added, the title goes to the main library.

The downloads folder is one folder, under Settings, General, Downloads. A download filed onto a different disk is copied there and checked instead of moved, which takes longer for a large file.

## Mounting a share

How to mount a share is the system's business, not Spool's. What matters to Spool is that it is mounted at the same path each time and comes back by itself after a restart.

- **Linux.** A line in `/etc/fstab` with the `nofail` and `_netdev` options, or a systemd automount.
- **macOS.** An automount map (`/etc/auto_master` and a map file), which mounts the share when it is first used and does not depend on anyone being signed in. A share connected from Finder lasts only as long as that sign-in.

  macOS guards network volumes as a matter of privacy, and without leave a program that lists one simply waits, with no error. Give Spool's program **Full Disk Access** under System Settings, Privacy & Security, or it will find the folders there and never be able to read them. The same applies to setting the mount up over SSH: Remote Login has its own "Allow full disk access for remote users", without which `/etc/auto_master` cannot be changed even by an administrator. "Check this location" names this when it is the cause.
- **A container.** Mount the share on the host and pass it in as a volume, or use the container system's own NFS or SMB volume.

If the share comes from a NAS that can run a container, [a helper](helper.md) on the NAS downloads and files there directly, and moves titles between its disks without the network.

NFS suits a share that is only read. If Spool is to manage the location, the user Spool runs as needs to be able to write there.

## Limits

- New downloads for a title go to its own folder only. Copies elsewhere are found by looking through a location, not made by Spool.
- Looking through folders is offered for further locations, not for the main one. A main library comes in through `spool migrate` or from Plex; see [Your library and what Spool does to it](your-library.md#bringing-in-a-library-you-already-have).
- Each location has its own recycle folder, so recycling never copies a file from one disk to another.
