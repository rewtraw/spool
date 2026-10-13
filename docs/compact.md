# Compact versions

A compact version is a smaller copy of a film or an episode that Spool makes itself and keeps beside the original. It is for playing away from home, over a slow connection, or on a device with little room: a 47 GB film on disc becomes about 5 GB at 1080p. The original is never touched.

This part of Spool is new. It is tested end to end with small files, and timed on one real machine; few compact versions have been watched yet.

## What one is

| | The original | Its compact version |
| --- | --- | --- |
| Picture | As it came | HEVC, made to fit a screen of the height you choose, at the rate you choose. Ten-bit and HDR10 are kept where the original has them. |
| Sound | As it came | Sound that is already small (AAC, Dolby Digital, Dolby Digital Plus) is copied. The rest, such as TrueHD and DTS-HD, becomes Dolby Digital Plus in 5.1. Every sound track is kept. |
| Subtitles, chapters, fonts | As they came | Carried over as they are. |
| File | Its own name | The same name with ` - Compact 1080p` on the end, in a Matroska file, in the same folder. |
| To Spool | The title's file | A version of it. Searches, upgrades and size targets go on looking at the original. |

Not carried over:

- **Dolby Vision.** A file that is Dolby Vision over an HDR10 layer comes out as HDR10. One that is Dolby Vision with nothing under it is left alone, with the reason: it cannot be re-encoded without its colours coming out wrong.
- **Atmos and other object sound.** It becomes plain 5.1.
- **Subtitle files beside the video.** Subtitles inside the file are kept; a subtitle file Spool or you put beside the original is offered with the original only.

## Making them

**One film.** On its page, under the file: **Make a compact version**, with roughly how big it will be. A file that would not gain from one says why.

**A series.** On its page: **Make compact versions**, which takes every episode that would gain from one.

**Several titles.** In the library, select them and choose **Make compact versions** in the bar.

**The whole library.** Settings, Compact versions shows how many files would gain from one, how much room they would take and how much film that is to get through, with a button to put them all in line.

**Whatever arrives.** The same tab has "Make one of whatever arrives". Each download that is filed is then put in line, if it would gain from one.

They are made one at a time, in the background. Activity shows the one being made, how far it has got and how fast, and those waiting. Any of them can be cancelled there, and a cancelled one leaves nothing behind.

A file is passed over when it is less than half again as big as its compact version would be, so a small film is not re-encoded for nothing.

## How long, and with what

Spool uses ffmpeg, and the machine's video hardware where ffmpeg offers it. The settings tab says which it found.

| Machine | From a 4K film on disc to 1080p | A two-hour film takes |
| --- | --- | --- |
| A Mac with Apple silicon, on its video hardware | About ten times the speed it plays | About 12 minutes |
| The same, keeping the picture at 4K | About two and a half to four times | 30 to 50 minutes |
| On the processor alone | About one and a half times, less on a small machine | Over an hour |

Measured on a Mac mini with an M4. The video hardware does the reading, the resizing and the writing, and the processor is left nearly idle; when it cannot read a file, Spool falls back to the processor for the reading, and then, if you allow it, for everything.

On Linux Spool encodes on the processor.

## More machines to make them

Making compact versions is the one job that goes faster with more computers. Any Mac in the house with Apple silicon can make them alongside the machine Spool runs on, and each one added makes about as many again.

Such a machine holds none of the library and needs no shared folder. Spool gives it one film at a time. It reads the film from Spool over the network, makes the compact version on its video hardware, and Spool brings the result back, checks it and files it as it would one made at home. Spool's own machine goes on making its own meanwhile. Activity shows each film with the machine making it.

**To add one**, from a checkout of Spool on any Mac:

```
deploy/maker.sh the-other-mac     # an SSH host, or "local" for the Mac you are sitting at
```

It installs Spool there to start with the machine, allowed only to make, and prints a code. In Spool, under Settings, Library, the machine appears as found on the network: type the code and the two are linked. The machine needs ffmpeg (`brew install ffmpeg`) and room for one compact version at a time, a few gigabytes.

**When it is given work** is set on its card in Settings, Library: whenever there is any, overnight only (eleven until seven), or never. Overnight suits a machine somebody works at.

What it is trusted with is small. It is started allowing `make` and nothing else, so it downloads nothing, files nothing and is shown nothing of the library. It reads only the one film it has been given, on a ticket made out for that film, and only from the machine that asked. It keeps what it makes in a folder of its own until Spool has taken it.

If it is switched off, asleep or restarted part-way, the film goes back in the line and another machine takes it; the machine is left alone for ten minutes and then offered work again. A film it finds it cannot make is reported as it would be at home.

Only video hardware is used this way. A linked machine without it is given nothing, since sending a film across the network to be made slowly gains nothing. That includes a [helper](helper.md) on a NAS, which is why turning this on changes nothing there.

## The settings

Under Settings, Compact versions.

| Setting | Meaning |
| --- | --- |
| Picture height | The picture is made to fit a screen this high: 1080 fits within 1920 by 1080. A picture that fits already is not made bigger. |
| Picture, kilobits a second | 8,000 is a good 1080p. A two-hour film at 8,000 comes to about 8 GB; at 4,000, about 4 GB. |
| Surround sound, kilobits a second | The rate for sound that has to be converted. 640 is the most Dolby Digital Plus is usually given. |
| Use the processor when the video hardware cannot | Off, a file the hardware cannot do is reported and left. |
| For titles on a location Spool only reads | Where their compact versions go. See below. |
| Make one of whatever arrives | Puts each filed download in line. |

Changing the settings does not remake what has been made. Delete a compact version and make it again for that.

## Where they are kept

Beside the original, wherever Spool may write.

A title on [a location Spool only reads](locations.md#managed-or-only-read) cannot have anything put beside it. Choose a location for the compact versions of such titles, and each is put there in a folder of the title's name, which becomes [one more place the title is held](locations.md#one-title-in-two-places). With none chosen, those titles get none.

That has a use of its own: with the archive on a NAS that is switched off, a title whose compact version is on a disk that is on can still be played.

## Playing one

A player is offered the original first and the compact version as a version of it, named **Compact 1080p**. In Infuse, hold Play and choose **Version**. Nothing is chosen for you: a player away from home has to be told to take the smaller one.

Plex and Jellyfin, reading the same folders, should take a film's two files for versions of one film by the way they are named; that has not been tried.

## Keeping only the compact version

On a film's page, a compact version has **Keep only this**. The original is deleted, as any file is: it goes to the [recycle folder](your-library.md#getting-something-back) for as long as the settings keep things there. The compact version becomes the film.

This is the way to make room without losing a title. Because the compact version is then the title's file, a monitored title may be upgraded again by a later search; stop monitoring it, or give it a profile that is content with what it has.

## What happens when things change

- **The original is replaced** by an upgrade: its compact version was made from a file that is gone, and is removed. With "Make one of whatever arrives" on, a new one is made from the new file.
- **The original is deleted**: its compact version goes with it.
- **A compact version is deleted**: only that file goes. It is not kept in the recycle folder, since it can be made again.
- **Spool is restarted**: the one being made is started again from the beginning, and the line carries on.
- **The database is lost**: a compact version is known by its name, and is tied again to the file it was made from when the folder is read.

## Limits

- Films and episodes only, one compact version of each file.
- A film that has several cuts gets a compact version of its main file.
- Other machines make them only on a Mac's video hardware. A Linux or Windows machine with a graphics card cannot yet be used.
- Nothing is converted while it is played. A player asking for a smaller stream of a file that has no compact version gets the original.
