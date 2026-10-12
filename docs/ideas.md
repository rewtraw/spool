# Ideas

Things Spool could do that have not been built. Nothing here is promised; each entry says what it would give, what it would take, and why it is waiting.

## A faster sync for Infuse's Library Mode

**What it is.** Infuse has two ways of using a Jellyfin server. In Direct Mode, the default and the one Spool is tested with, it asks the server for each screen as you browse. In Library Mode it copies the whole catalogue onto the device and browses that copy, which gives Infuse's own home screen, lists and filters, and lets it merge several servers into one library.

**What is missing.** To keep that copy current, Infuse looks for a Jellyfin plugin called InfuseSync, which answers "what changed since I last looked?". Spool does not answer it, so Infuse falls back to reading the whole library again through the ordinary routes. That works, and is only slower.

**What it would take.** Six routes under `/InfuseSync`:

| Route | Purpose |
| --- | --- |
| Checkpoint, StartSync | Mark a point in time and open a sync from it |
| UpdatedItems, RemovedItems | The titles, seasons and episodes that changed or went since that point |
| UserData | Watched state and progress that changed since that point |
| UserFolders | The libraries this sign-in can see |

Spool already records what these need: the change count described in [architecture.md](architecture.md#knowing-what-changed) covers titles, episodes, files and watched state, removals included. What remains is the routes themselves, about a day of work.

**Why it is waiting.**

- Nobody has asked for Library Mode yet, and a full read of the library takes Spool milliseconds, so the gain may be too small to notice.
- The protocol is private. It is not in Jellyfin's published API, so the routes would be copied from the plugin's source (GPL v3, as Spool is) and could not be checked against a schema. An Infuse update could change what it expects.
- It can only be tested with a real Apple TV in Library Mode, over several rounds of adding and removing things.

**When to build it.** Someone uses Library Mode, prefers it, and finds the sync after new downloads slow.

## More for the helper to do

**What it is.** [A helper](helper.md) on the machine that holds the disks already downloads, unpacks, files and moves there. Three jobs it could also take are still done by Spool across the share.

| Job | Today | With the helper doing it |
| --- | --- | --- |
| Reading each new file's details | A little of each new file read across the network | Read on the NAS; only the findings cross |
| Checking subtitles against the sound | The sound track read across the network | On the NAS |
| Playing | NAS to Spool's machine to the player | From the NAS to the player directly; see [Linked Spools](#linked-spools) |

It would also remove the need for the helper's downloads folder to be shared with Spool's machine, since Spool would no longer read a download before filing it.

**What it would cost.** Each is a part of Spool that reads files directly today and would have to ask the helper instead: the import plan, the reading of a file's details and the subtitle check. (Listing what is in a folder is already the helper's; see [Looking through its own folders](helper.md#looking-through-its-own-folders).) Playing from the NAS means the helper serving files to players, which gives it a door to the outside that it does not have now.

**Why it is waiting.** The jobs that move whole films across the network are the ones that were slow, and those are done. These read a little of each file.

**When to build it.** A nightly scan of a large share proves slow in practice, or sharing the downloads folder is in the way.

## Linked Spools

**What it is.** One idea in place of three. A [helper](helper.md) on a NAS, a NAS that plays its own films, and a library shared with another household were written up here as separate things. They are the same thing at different levels of trust: one Spool linked to another, with what the link allows as the only difference. This is being built, in the stages below. The first three are done: see [Playing from the helper](helper.md#playing-from-the-helper) and [Looking through its own folders](helper.md#looking-through-its-own-folders).

**What a link allows.** Two Spools are linked with a code. Each then grants the other some of three things:

| Grant | What the other Spool may do |
| --- | --- |
| See | List what is in the folders that are shared with it. |
| Play | Fetch one file, on a ticket. |
| Work | Download, file, move and convert on this machine's disks, as it directs. |

| What it is called today | The grants |
| --- | --- |
| A helper on a NAS | See, Play and Work, given to the Spool that drives it. |
| A friend's Spool | See and Play, given both ways or one way. |
| A relative's machine that you look after | All three. |

A location stops meaning folders on this machine and comes to mean folders on some linked Spool. A disk plugged in here, a NAS and another household are then one idea.

**Machines and locations.** Two things, where the settings have three today. A machine is this one or a linked Spool: an address, a key, what it allows, and the players sent to it. A location is a pair of library folders, for films and for series, on exactly one machine. The main folders are the first location on this machine. What a link holds today as a pair of matching folders, one as this machine mounts the share and one as the other knows it, moves onto each location, and the mounted folder becomes something a location may have, used only by what still reads across a share. A location on a friend's Spool has none. The settings page already shows it this way: one Library section, each machine with the locations on it, a location being on the machine whose disks its folders are mounted under. The rest is built with stage 4.

**The one rule.** Every folder has exactly one Spool that decides for it. Others may see what is in it, play from it, or work in it when told. None of them chooses what is downloaded, upgraded or deleted there. A helper needs that rule so that two machines never file the same thing, and sharing needs it so that nobody changes someone else's library.

**Who enforces a grant.** The side that gives it, as a helper holds itself to the folders it was started with today. A linked Spool cannot widen what it was given.

**The ticket.** Playing from another machine rests on a ticket: it names one file, runs out after a few hours, and is signed with a key the two Spools share. The machine that holds the file serves it to whoever holds the ticket and to nobody else. It has no sign-in and nothing to browse, so a player still knows one server.

**The stages.**

| Stage | What is built | What it gives |
| --- | --- | --- |
| 1 | Links. "Helpers" in the settings becomes "Linked Spools", each link with its grants, speaking a protocol with a version, so that two Spools on different releases still understand each other. | What a helper does today, on the footing the rest stands on. |
| 2 | Play. The machine that holds a file serves it on a ticket. Spool sends a player on the same network there, and goes on relaying for a player elsewhere. | At home, a film on the NAS comes from the NAS. |
| 3 | See, done where the disks are. The linked machine reads its own folders and reports what it finds. | Looking through a location and the nightly scan stop reading across the share. |
| 4 | Machines and locations, and finding and linking. The settings for folders, locations and linked Spools become one list: each machine, and under it the locations on it. A location names the machine it is on and its folders there, in place of Spool working that out from where a share is mounted. A Spool announces itself on the network, is listed as found, and is linked with a short code; the two agree a key between themselves. A linked machine reports in, and is no longer asked how it stands every second. | One place to say where the library is. No keys to copy, and no folder typed twice. |
| 5 | An account for each person who plays. | Each has what they have watched, and limits can be set; someone without a Spool of their own can be given an account. |
| 6 | Links between households: See and Play, with limits set on the link. | Each household sees the others' libraries in its own. |

**Limits on a link to another household.** Which locations are shared; whether the films as they are or only their [compact versions](compact.md); how many may play at once; how much may be sent at once; and an end to the link at any time. A film is sent at the rate it was made at, so from a home whose line is slow upstream, compact versions are what to share.

**Whose accounts.** People sign in to their own household's Spool. A link is between two Spools, so one never holds accounts for the other's people, and learns of their watching only that a file was asked for.

**What is not known.**

- Whether players other than Infuse follow being sent to another address for a film. Infuse 8.5 on a Mac does, and seeks there: it was tried on 12 October 2026. It asked Spool once and then kept to the address it was sent to. Infuse on an Apple TV and on a phone are expected to be the same and have not been tried.
- How to tell that a player reached over a private network such as Tailscale is at home. Spool sees such a player at its private-network address, which says nothing of where it is, so a home range does not cover it and it has to be named by that address.
- Whether a player copes with one server at two addresses. Infuse did not, when a second address was added to a share by hand.

**What stands in the way.**

- A player away from home cannot reach a NAS that is only on the home network. Spool sends a player there only when the two are on the same network.
- A container on its container system's own network neither hears nor is heard by announcements on the home network. To be found, it has to share the host's network.
- On a Mac, a program run as a service may not speak to the local network until it is allowed to under Privacy & Security, and is refused without a word.
- Anything on a network can announce itself. The code is what keeps a stranger's machine from being linked, so linking is never a single click.
- Two homes cannot reach each other without help: a private network between them, or an open port with a certificate.
- Other households update when they like. What two Spools say to each other has to go on being understood by older and newer releases.

**The line not to cross.** Links between households are by invitation, between people who know each other, as a library is shared with family. There is no list of Spools to browse and no way to find a stranger's.

**What was considered and left out.**

- **A second Spool that takes over by itself when the first stops.** Two machines must never both believe they are in charge, or each downloads, files and deletes the same things. For one household that risk buys back a few seconds.
- **A second Spool that keeps a copy of the catalogue, to be switched to by hand.** Not needed: when a linked machine is away its titles go offline and the rest carries on, and the nightly backup restores a catalogue onto another machine.
- **Work sent to whichever machine is free.** Worth having only with a second machine that does Work.

**What may follow.** A NAS that runs a whole Spool of its own, deciding for its own folders, with the household's settings, accounts and what has been watched kept the same on each. Either machine would then go on serving players while the other is down. Settings are small and seldom change, so the latest edit can win; the catalogue is not copied, because each title belongs to the Spool whose folder holds it. It would mean splitting a Usenet provider's connections between the two, and each reading the indexers for itself. It waits until links have been in use.

## Converting while it plays

**What it is.** [Compact versions](compact.md) are made ahead of time. A player that asks for a smaller stream of a file with no compact version gets the original. Spool could instead convert the file as it is played, as Jellyfin and Plex do, and Infuse can ask for exactly that since its version 8.5.

**What it would take.** Starting ffmpeg for each player that asks, cutting what it makes into short pieces, serving those in the form players expect (HLS), following the player when it skips about, and clearing up when it stops. The part of the players' protocol that asks for a converted stream would have to be answered, where today Spool says every file is to be played as it is.

**What is known.** A Mac mini with an M4 turns a 4K film on disc into 1080p at about ten times the speed it plays, on its video hardware, and two at once each get half of that. So one such machine could serve eight or so players at once, each with a film converted as it goes.

**Why it is waiting.** A compact version made once serves every player from then on and survives a restart; converting as it plays serves one player once. For a library whose titles are known ahead of time, making them ahead of time is less to build and less to go wrong.

**When to build it.** Titles are often played away from home before anyone thought to make a compact version of them; or the library is shared with people on connections of every kind, as in [links between households](#linked-spools).

## Titles worth having, found without being asked for

**What it is.** Spool looks for what it has been told to look for. With an indexer that sets no limit on how much it may be asked, it could also keep an eye out for films worth having that nobody has added: new ones that are being talked about, and old ones that are well thought of, each at a quality the profile would accept.

**How it would go.**

1. A list of films to consider is kept up from sources that say how a film is regarded: TMDB's ratings and counts of votes, which Spool reads already; lists such as Letterboxd's highest rated or most popular; and what is trending this week.
2. Rules say which of them are wanted: a rating above some mark (a Letterboxd average over 3.5, say) with enough votes to mean something, within years and kinds you choose, leaving out what you have turned down before.
3. Each film that passes is offered. Either it waits on a page to be accepted or turned down, or, if you say so, it is added, monitored and looked for like any other.

**What it needs.**

- Somewhere to read ratings. TMDB's are there now. Letterboxd has no open API; its lists can be read through TMDB ids that others publish, or it can be left out.
- A cap. Without one a rule such as "every film rated over 3.5" is tens of thousands of films. So many a week, so much room in all, and nothing added while the disk is short.
- A memory of what was turned down, so the same film is not offered every week.
- A place to put them. A location of their own, so that what was found for you can be told from what you asked for and cleared out together.

**Why it is waiting.** It is a different thing from the rest of Spool, which does what it is asked. Left to add by itself it can fill a disk with films nobody watches; kept to offering, it is a list to work through. Which of the two is wanted decides most of the design.

**When to build it.** With room to spare on a location and an indexer that does not count requests, and once it is settled whether it should offer or add.
