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
| Reading a folder and each file's details | Every file opened across the network when a location is looked through or scanned | Read on the NAS; only the findings cross |
| Checking subtitles against the sound | The sound track read across the network | On the NAS |
| Playing | NAS to Spool's machine to the player | From the NAS to the player directly; see [below](#playing-straight-from-the-helper) |

It would also remove the need for the helper's downloads folder to be shared with Spool's machine, since Spool would no longer read a download before filing it.

**What it would cost.** Each is a part of Spool that reads files directly today and would have to ask the helper instead: the import plan, the library scan and the subtitle check. Playing from the NAS means the helper serving files to players, which gives it a door to the outside that it does not have now.

**Why it is waiting.** The jobs that move whole films across the network are the ones that were slow, and those are done. These read a little of each file.

**When to build it.** A nightly scan of a large share proves slow in practice, or sharing the downloads folder is in the way.

## Playing straight from the helper

**What it is.** A film kept on a NAS is played today by way of Spool's machine: the NAS sends it to Spool across the share, and Spool sends it on to the player. With [a helper](helper.md) on the NAS, the player could fetch the film from the NAS itself, while still knowing only one server.

**How it would work.**

1. The player signs in to Spool and browses the library there, as now. Spool is the only server it is ever told of.
2. Asked to play a file that lies on a helper's disks, Spool answers with a redirect to the helper, carrying a ticket for that one file.
3. The player follows the redirect and takes the film from the NAS. What was watched, and where it stopped, are still reported to Spool.

**Why the player still sees one server.** The helper would not answer the question players ask of a network to find servers, and would have no sign-in, no library and nothing to browse. It would serve a file to whoever holds a ticket from Spool, and nothing else. No name in DNS is needed for it: the redirect carries its address.

**The ticket.** It names one file, runs out after a few hours, and is signed with the key Spool and the helper already share, so the helper cannot be used as an open door to the files.

**What is not known.**

- Whether Infuse follows a redirect on the address it streams from, and keeps following it when seeking. Most players do. If it does not, the protocol has a second way to say that a file is at another address (a media source marked as remote, with its own address), to try next.
- How the player is told from the helper's side that a file has subtitles beside it. Spool hands those out itself today, and could go on doing so.

**What it would cost.**

- The helper takes requests from players, where today it takes them only from Spool. It would stay on the home network and answer only to a ticket, but it is a door it does not have now.
- A player away from home cannot reach a NAS that is only on the home network. Spool would redirect a player on the same network as the helper and go on relaying for any other, which means knowing which is which from the address the request came from.

**Why it is waiting.** On a gigabit network, relaying a film through Spool's machine costs next to nothing: a film plays at a small fraction of the line's speed.

**When to build it.** Spool's machine becomes the limit on playing, for instance with several films at once of the largest kind; or the NAS is given a faster line than Spool's machine has; or playing what is on the NAS should carry on while Spool's machine is busy.

## From one Spool and its helpers to a cluster

**What it is.** Spool with [helpers](helper.md) is already the simplest kind of cluster: one machine that decides and others that do the work where the disks are. This is what it has of one, what it lacks, and the steps between, in the order they would be worth taking.

**What is there now.**

| Part of a cluster | How Spool has it |
| --- | --- |
| One machine decides, others work | Spool holds the catalogue and makes every choice; a helper does what it is told. |
| More than one worker | The settings take any number of helpers, each serving the folders it is given. |
| Work goes to where the data is | A download runs on the machine that holds the disks it will be filed on. |
| A worker may fail | With a helper away, Spool does its work itself, and waits for what the helper had under way. |
| The parts agree on how to talk | Spool and a helper check that they are the same version before working together. |

**What is missing.**

| Part of a cluster | Today |
| --- | --- |
| A second machine that can decide | There is one Spool. While it is down nothing is searched for, chosen or played, though a NAS may hold most of the files. |
| A choice between workers | Each folder belongs to one helper. Nothing shares work out, or picks the machine with least to do. |
| A catalogue in more than one place | It is one database file on Spool's machine. Helpers hold none of it. |
| Workers that report in | Spool asks each helper how it stands, once a second. A helper never says of its own accord that it is there or what it has. |

**The steps, nearest first.**

1. **The helper reads its own folders.** Looking through a location and the nightly scan are done on the NAS, and only the findings cross. Written up under [More for the helper to do](#more-for-the-helper-to-do).
2. **The helper plays its own files.** A player takes a film from the NAS, on a ticket from Spool. Written up under [Playing straight from the helper](#playing-straight-from-the-helper). After this, playing what is on a NAS needs Spool only to sign in and browse.
3. **Helpers report in.** A helper is given Spool's address and tells it when it starts, what it has finished and when it is going down. Spool stops asking every second, and a second helper becomes a matter of starting it. [Being found and linked by itself](#helpers-that-are-found-and-linked-by-themselves) belongs with this.
4. **Work that can go anywhere, goes to whoever is free.** Checking subtitles against the sound and repairing a damaged download need a processor more than a particular disk. With more than one helper, Spool could send such a job to the one with least to do.
5. **A second Spool that follows the first.** It keeps a copy of the catalogue up to date and takes over when the first stops answering: searching, choosing, serving the web app and players. This is the step that makes a cluster in the strict sense.

**What the last step would cost.** It is of a different size from the others.

- The catalogue is SQLite, which one program writes to. Following it from another machine means shipping its changes across as they happen, or moving to a database made for more than one writer; both are large changes to how Spool keeps everything.
- Two machines must never both believe they are in charge, or each will download and file the same things. Deciding which one is, and being sure the other knows, is the hard part of every such system.
- Players and browsers would need one address that follows whichever is in charge.
- Everything Spool keeps in memory and not in the database (the line of titles waiting to be moved was one such, until it was written down) would have to be found and written down, or be lost when the other takes over.

**Why it is waiting.** For one household's library, Spool being down for the length of a restart costs a few seconds of nothing being searched for. The first two steps bring most of what a cluster would, at a fraction of the work: with them, a NAS goes on playing what it holds whatever Spool's machine is doing.

**When to build it.** Steps 1 and 2 when their own entries say. Step 3 when there is a second helper. Step 4 when a second helper sits idle while the first is busy. Step 5 if Spool's machine proves unreliable enough that its being away is noticed, and not before.

## Helpers that are found and linked by themselves

**What it is.** Setting up [a helper](helper.md) today means making a key, giving it to the helper and to Spool, typing the helper's address, and naming one folder as each machine sees it. A helper could instead announce itself on the network, be listed in Spool's settings as found, and be linked with a button and a short code.

**How it would go.**

1. A helper is started on a machine, with the folders it may use.
2. Under Settings, General, Helpers, a line appears: found on your network, with the machine's name and a **Link** button.
3. Linking asks for a short code the helper printed when it started. The two agree a key between themselves; nobody copies one.
4. Spool works out which of its locations lie on the helper's disks, and shows the match to be confirmed.

**The three parts.**

| Part | Today | As it would be |
| --- | --- | --- |
| Finding | The helper's address is typed in. | The helper announces itself on the network and Spool listens, as players already find Spool. |
| Trusting | A key is made by hand and given to both. | A helper that is not yet linked accepts one thing only: a request to link, with the right code. |
| Matching folders | One folder is named twice, as each machine sees it. | Spool compares what is in its own location folders with what the helper sees in its folders and lines them up, which is the comparison "Check this location" already makes. |

**What it would not change.**

- The folders a helper may touch are still set where the helper runs. That is what keeps it inside them, and it should not be something a button in a web page can widen.
- The helper's downloads folder still has to be shared with Spool's machine, until [the helper reads its downloads itself](#more-for-the-helper-to-do).
- Typing an address stays possible, for a helper that cannot be found.

**What stands in the way.**

- A container on its container system's own network neither hears nor is heard by the home network's announcements. The helper would have to share the host's network, as Spool must to be found by players from a container.
- On a Mac, a program may not speak to the local network until it is allowed to under Privacy & Security, and a Spool run as a service is refused without a word. Finding helpers would fail the same quiet way as reaching one does.
- Anything on the network can announce itself as a helper. The code is what keeps a stranger's machine from being linked, so linking can never be made a single click.
- Folders are matched by what is in them. Two empty folders look alike, and so do two copies of one library; those have to be chosen by hand.

**What it would cost.** About a day: the announcement and the listening are small, and most of the work is the linking and the matching, with their tests.

**Why it is waiting.** A helper is set up once. The four things typed today take a few minutes, and the settings page says at once whether they were right.

**When to build it.** With [helpers that report in](#from-one-spool-and-its-helpers-to-a-cluster), since both are a helper making itself known; or when a second helper is added and setting one up is no longer something done once.

## Converting while it plays

**What it is.** [Compact versions](compact.md) are made ahead of time. A player that asks for a smaller stream of a file with no compact version gets the original. Spool could instead convert the file as it is played, as Jellyfin and Plex do, and Infuse can ask for exactly that since its version 8.5.

**What it would take.** Starting ffmpeg for each player that asks, cutting what it makes into short pieces, serving those in the form players expect (HLS), following the player when it skips about, and clearing up when it stops. The part of the players' protocol that asks for a converted stream would have to be answered, where today Spool says every file is to be played as it is.

**What is known.** A Mac mini with an M4 turns a 4K film on disc into 1080p at about ten times the speed it plays, on its video hardware, and two at once each get half of that. So one such machine could serve eight or so players at once, each with a film converted as it goes.

**Why it is waiting.** A compact version made once serves every player from then on and survives a restart; converting as it plays serves one player once. For a library whose titles are known ahead of time, making them ahead of time is less to build and less to go wrong.

**When to build it.** Titles are often played away from home before anyone thought to make a compact version of them; or the library is shared with people on connections of every kind, as in [sharing between households](#sharing-a-library-between-households).

## Sharing a library between households

**What it is.** Several people each run their own Spool, and each sees the others' libraries in their own, as one. A title only one of them has is played from that one's machine, through the player's own Spool, so a player still knows one server.

**How it fits what is there.** Another household's Spool looks like [a location that is only read](locations.md#managed-or-only-read), reached over the internet and not through a mount.

| Wanted | Already there |
| --- | --- |
| Titles hidden while their owner's machine is off | A location that cannot be reached takes its titles offline. |
| One entry for a film two people have | A title in more than one place, each offered to players as a version. |
| Nothing of someone else's ever changed | Locations that are only read. |
| A player sees one server | The door players use; the other Spool is only where the film comes from. |
| The same film known as the same | Every Spool keeps the same ids for a title. |

**What is new.** Two Spools linked by invitation, with a code, each choosing what it shares; one asking the other what it has; and one fetching a file from the other to hand to a player.

**What stands in the way.**

- A film is sent at the rate it was made at. From a home whose line is slow upstream, a film on disc will not play. [Compact versions](compact.md) are the answer that exists: share those.
- Two homes cannot reach each other without help. A private network between them (Tailscale's sharing of one machine is the least to set up) or an open port with a certificate.
- What each owner must be able to set: which locations are shared, how much may be sent at once, how many players at once, and an end to the arrangement.
- What one household learns of another's watching. No more than that a film was sent.

**The line not to cross.** By invitation, between people who know each other, as a library is shared with family. No list of Spools to browse and no way to find a stranger's: that would be something else.

**When to build it.** Someone to share with runs Spool too.

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
