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
