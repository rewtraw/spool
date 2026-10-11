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

## A helper on the machine that holds the disks

**What it is.** Spool runs on one machine and may keep part of its library on another, a NAS, reached over a network share. Everything Spool does to files there crosses the network, twice or more for anything that has to be repaired or unpacked. A helper would be a small program on the NAS itself that does only the file work, told what to do by Spool. Spool stays the one place that knows the catalogue, makes the choices and serves the web app and players.

**What the helper would do.**

| Job | Today, over a share | With a helper |
| --- | --- | --- |
| Download | Bytes arrive at Spool's machine and are written across the network | Fetched from Usenet by the NAS, written to its own disks |
| Repair and unpack | Read from the NAS and written back, both across the network | Local |
| Filing into the library | A copy across the network when downloads are staged elsewhere; a rename only if the downloads folder is in the same dataset | Always a local move |
| Reading a folder and each file's details | Every file opened across the network | Local; only the findings cross |
| Checking subtitles against the sound | The sound track read across the network | Local |
| Playing | NAS to Spool's machine to the player | Could go from the NAS to the player directly |

It would also remove the dependence on the share being mounted on Spool's machine for anything but playing, and with that the Mac's privacy consent for network volumes.

**How it would be built.**

- The download engine is already a crate of its own (`spool-nntp`), with no knowledge of the catalogue. The helper is that engine, the import step's file moves, and the folder survey, behind a small HTTP API with a key.
- Spool gains a second kind of engine: one it drives over HTTP where today it drives one in its own process. A location names the helper that serves it; a location without one works as now.
- Jobs are owned by the helper and reported back, so a restart of either side resumes. The import journal stays with Spool: the helper is told each move and confirms it.
- It ships as the container image Spool already has, started in a helper mode, which is how it would run on TrueNAS SCALE and similar.

**What it would cost.**

- A second thing to install and keep at the same version as Spool, and an API between them that has to stay compatible or refuse clearly when it is not.
- The server calls the engine directly in many places today. Putting an interface between them is the bulk of the work: two to three days, with the end-to-end tests run against both kinds.
- Usenet credentials have to be given to the helper, so it holds secrets too.

**Why it is waiting.** A managed location on a NAS works without it, more slowly. How much more slowly is not yet measured on a real network, and the answer decides whether this is worth a second moving part.

**When to build it.** Downloads onto a share prove slow or fragile in practice; or the machine Spool runs on is short of disk to stage downloads and the NAS is the only place with room; or playing through Spool's machine becomes the limit.
