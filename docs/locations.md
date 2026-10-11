# A library in more than one place

Spool always has a main location: the Movies and Series folders under Settings, General, Folders. Beside it you can add further locations, each with folders of its own: another disk, or a share from a NAS mounted on the machine Spool runs on. The library shows them as one.

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
2. Under Settings, General, More locations, choose **Add a location** and fill in:

   | Field | What to put |
   | --- | --- |
   | Name | What to call it, such as the NAS's name. |
   | Movies, Series | The folders that hold them. Either may be left empty. |
   | Disk or share that must be mounted | The mount point, such as `/Volumes/Goliath`. Leave it empty if the folders only have to be readable. |
   | Let Spool change files here | Off for only read. |

3. **Save.** The location now shows whether it can be reached, how many titles are on it and how much room is free.

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

A title that is in the library **without a file**, while the folder here has one, is listed under To add. This is what a library looks like after coming over from Radarr and Sonarr: the catalogue came across, and the files are where they always were. Adding does not make a second copy of the title; it points the one you have at the folder that holds its file, and keeps its monitoring and profile.

Then **Add to the library**. On a managed location you can choose to monitor what is added, so that Spool looks for upgrades and missing episodes; leave that off for a library you want left as it is. A location that is only read does not offer the choice, since nothing there is ever searched for.

After adding, a title whose file runs much longer or shorter than the film it was matched to is noted, so a wrong match stands out.

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

Once there is a managed location beside the main one, "Put new titles on" chooses which receives titles you add. Titles already in the library stay where they are. If the chosen location cannot be reached when a title is added, the title goes to the main library.

The downloads folder is one folder, under Settings, General, Folders. A download filed onto a different disk is copied there and checked instead of moved, which takes longer for a large file.

## Mounting a share

How to mount a share is the system's business, not Spool's. What matters to Spool is that it is mounted at the same path each time and comes back by itself after a restart.

- **Linux.** A line in `/etc/fstab` with the `nofail` and `_netdev` options, or a systemd automount.
- **macOS.** An automount map (`/etc/auto_master` and a map file), which mounts the share when it is first used and does not depend on anyone being signed in. A share connected from Finder lasts only as long as that sign-in.
- **A container.** Mount the share on the host and pass it in as a volume, or use the container system's own NFS or SMB volume.

NFS suits a share that is only read. If Spool is to manage the location, the user Spool runs as needs to be able to write there.

## Limits

- A title lives in one location. If the same film has a file on two, the second is reported as already in the library and skipped.
- Looking through folders is offered for further locations, not for the main one. A main library comes in through `spool migrate` or from Plex; see [Your library and what Spool does to it](your-library.md#bringing-in-a-library-you-already-have).
- Each location has its own recycle folder, so recycling never copies a file from one disk to another.
