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
| Playing | NAS to Spool's machine to the player | From the NAS to the player directly |

It would also remove the need for the helper's downloads folder to be shared with Spool's machine, since Spool would no longer read a download before filing it.

**What it would cost.** Each is a part of Spool that reads files directly today and would have to ask the helper instead: the import plan, the library scan and the subtitle check. Playing from the NAS means the helper serving files to players, which gives it a door to the outside that it does not have now.

**Why it is waiting.** The jobs that move whole films across the network are the ones that were slow, and those are done. These read a little of each file.

**When to build it.** A nightly scan of a large share proves slow in practice, or sharing the downloads folder is in the way.
