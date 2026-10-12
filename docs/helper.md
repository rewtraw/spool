# A helper on the machine that holds the disks

Spool runs on one machine and may keep part of its library on another, such as a NAS, which it reaches over a [network share](locations.md). Without help, everything it does to files there crosses the network: a download arrives at Spool's machine and is copied over, and a title moved between two of the NAS's own disks is read to Spool's machine and written back.

A helper is a small Spool that runs on the NAS itself and does only the file work, on the NAS's own disks. The Spool you already have stays in charge: it holds the catalogue, makes every choice, and serves the web app and players. It tells the helper what to fetch and what goes where.

This part of Spool is new. It is tested end to end, with a helper and a Spool in one test, and it runs on one real setup: a Mac driving a helper on TrueNAS SCALE, where a download arrives at the speed of the line and is in the library seconds after it is unpacked. A move carried out by the helper, and a helper restarted in the middle of one, have had no real use yet.

## What the helper does, and what it leaves alone

| Job | Without a helper | With one |
| --- | --- | --- |
| Downloading for a title kept on the NAS | Fetched by Spool's machine, then copied across the network | Fetched from Usenet by the NAS, onto its own disks |
| Repairing and unpacking | On Spool's machine | On the NAS |
| Filing into the library | A copy across the network | A rename, or a copy from one of the NAS's disks to another |
| Moving a title between two folders on the NAS | Read to Spool's machine and written back | Disk to disk inside the NAS |
| Reading what is in the NAS's folders | Every folder listed across the network when a location is looked through or scanned | Listed by the NAS, and only what it found sent over; see [Looking through its own folders](#looking-through-its-own-folders) |
| Playing a film kept on the NAS, at home | NAS to Spool's machine to the player | From the NAS to the player, once you say which players; see [Playing from the helper](#playing-from-the-helper) |

Still done by Spool across the share, as before:

- reading the details of a file that is new to Spool: how long it runs, what video and sound are in it;
- subtitles, recycling and deleting;
- playing, for every player you have not named as one to send to the helper;
- everything on a location the helper does not serve.

A helper that cannot be reached takes nothing with it. Downloads for its part of the library run on Spool's machine and are filed across the share, as they would be without it; a notice says the helper is away; and what it was already downloading is waited for, not written off.

## What it may touch

A helper is started with a list of folders and works only inside them. Every path it is given is checked against the list.

| Setting | Meaning |
| --- | --- |
| `SPOOL_HELPER_KEY` | Sent by Spool with every request. At least 16 characters. Nothing is answered without it. |
| `SPOOL_HELPER_ROOTS` | Folders it may file into and move titles within, separated by colons. |
| `SPOOL_HELPER_READ_ROOTS` | Folders it may read and copy from. It never writes to them, moves anything out of them or removes anything from them. |
| `SPOOL_HELPER_DOWNLOADS` | The folder it downloads into. It clears away only finished downloads of its own there. |
| `SPOOL_HELPER_GRANTS` | What it does for whoever holds the key: any of `see`, `play` and `work`, separated by commas. Left out, all three. Without `work` it downloads, files and moves nothing; without `play` it hands no file to a player. |

Mount a folder it should only read as read-only in the container as well, so that the system refuses a write whatever is asked.

The helper keeps one file of its own, in `/data`: the Usenet servers and limits Spool gives it, so that downloads carry on after the helper restarts. It holds your Usenet passwords and is readable only by the helper's user.

## Setting one up

### 1. Start the helper on the NAS

It is the same container image as Spool, started with `helper`. [deploy/helper.compose.yaml](https://github.com/rewtraw/spool/blob/main/deploy/helper.compose.yaml) is the definition, for `docker compose` or for a custom app on TrueNAS SCALE:

```yaml
services:
  spool-helper:
    image: ghcr.io/rewtraw/spool:latest
    command: ["helper"]
    user: "568:568"                     # the user that owns the media
    ports: ["7978:7979"]
    environment:
      SPOOL_HELPER_KEY: "a long random key"
      SPOOL_HELPER_ROOTS: "/nas/movies:/nas/tv"
      SPOOL_HELPER_READ_ROOTS: "/nas/archive-movies"
      SPOOL_HELPER_DOWNLOADS: "/nas/downloads/spool"
    volumes:
      - spool-helper-data:/data
      - /mnt/tank/movies:/nas/movies
      - /mnt/tank/tv:/nas/tv
      - /mnt/tank/downloads:/nas/downloads
      - /mnt/archive/movies:/nas/archive-movies:ro
volumes:
  spool-helper-data:
```

**Name the folders in the container as Spool's machine names them.** Spool turns a path on its own machine into a path on the helper by swapping the start of it, and everything after the start has to match. If Spool's machine mounts the NAS's shares as `/Volumes/nas/movies`, `/Volumes/nas/tv` and `/Volumes/nas/archive-movies`, mount them in the container as `/nas/movies`, `/nas/tv` and `/nas/archive-movies`, wherever the NAS itself keeps them.

Put the downloads folder on the same dataset as the library if you can: a finished download is then filed by renaming it. On another dataset it is copied, inside the NAS.

### 2. Let Spool's machine see the downloads folder

Spool reads what was downloaded before it files it: which file is the film, how long it runs, what is in it. So the helper's downloads folder has to be shared and mounted on Spool's machine, beside the library folders and under the name it has in the container (`/Volumes/nas/downloads` for the example above). Reading is enough; the share can be read-only.

### 3. Tell Spool about the helper

Under Settings, Library, choose **Link another Spool**:

| Field | What to put |
| --- | --- |
| Name | What to call it, such as the NAS's name. |
| Address | Where it answers, such as `http://nas:7978`. |
| Key | The key it was started with. |
| The other machine's disks, as this machine sees them | The folder the shares are mounted under here, such as `/Volumes/nas`. |
| The same folder, as the helper sees it | The folder they are mounted under in the container, such as `/nas`. |

The key is not shown again once saved, and is not handed back to the browser.

The two folders must name the same place. A path is turned from one machine's to the other's by swapping them at its start: with the example above, `/Volumes/nas/movies/Heat (1995)` here is `/nas/movies/Heat (1995)` to the helper.

There is nothing to set on a location. Every location whose folders lie under the first folder is served by the helper, and the settings page says which those are.

### 4. Check

**Check this location**, on a location the helper serves, asks the helper whether it sees the same folder Spool does, and says whether Spool can see the helper's downloads. Anything that fails comes with what to do about it.

From then on, a download for a title on that location shows **On** and the helper's name in Activity, and nothing appears in the downloads folder on Spool's own machine. The Linked Spools section of the settings shows, for each helper, whether it answers, which locations it serves, how many downloads it is running and how much room it has for them.

### On TrueNAS SCALE

The helper is a custom app. Under Apps, Discover Apps, the menu beside Custom App offers **Install via YAML**: give it a name and paste the definition, with your datasets and a key of your own. From a shell on the NAS the same thing is one call:

```sh
midclt call -job app.create '{"custom_app": true, "app_name": "spool-helper", "custom_compose_config_string": "<the definition>"}'
```

Run it as the apps user (568), which is the user a share from TrueNAS usually maps everyone to, so that what the helper writes and what Spool writes across the share belong to the same owner. Share the helper's downloads folder over NFS, read-only, to the network Spool's machine is on.

## Playing from the helper

A film on the NAS is played, without a helper, by way of Spool's machine: the NAS sends it to Spool across the share, and Spool sends it on. A helper can hand the film to the player itself. Players still know one server, Spool, where they sign in, browse and report what was watched.

1. A player asks Spool for a film, as it always does.
2. If the film is on the helper's disks and the player is one you have named, Spool answers with the helper's address and a **ticket**.
3. The player takes the film from the helper.

The ticket names that one file, runs out after twelve hours, and is signed with a key made from the one Spool and the helper share. The helper hands a file to whoever comes with a ticket of Spool's and to nobody else; it has no sign-in and nothing to browse. It opens the file by going from the shared folder one name at a time and follows no link on the way, so a link placed in a shared folder leads nowhere. A library whose films are themselves links to somewhere else cannot be played from the helper; leave its players off the list below, and Spool goes on sending those films itself.

**Which players are sent.** None, until you say. Under Settings, Library, each linked machine has **Send players on the same network straight to it**. On, a player on the same network as the machine is sent to it; every other player, and every player while it is off, gets the film through Spool as before. The same network means the same stretch of addresses as the one the machine answers Spool from: the first three numbers of an address such as `192.168.1.20`.

**A player that reaches Spool over Tailscale** is seen at its Tailscale address, even at home. Spool asks Tailscale on its own machine where it is really talking to that player: when the two talk directly, the player is placed by that address, so it counts as on the network at home and not away. A player reached through one of Tailscale's relays is taken to be away and gets the film through Spool. If the linked machine is itself reached over Tailscale, every player on Tailscale counts as near it.

**A machine on both.** A linked machine that Spool reaches on the home network may be on your Tailscale as well. Spool looks for a device on its own Tailscale with the link's name, asks it what it is, and takes it for the same machine only if it answers as the link does. From then on a player away from home, on Tailscale, is sent to the machine's Tailscale address, and a player at home to its home address. The link's status says "also on Tailscale" when this has been found. The link's key is sent to that device to ask, so the name of a link should be the name of a machine of yours.

**Other players.** Under **How it is reached, and other players to send** there is a list of further addresses: ranges such as `10.0.5.0/24`, or one player's address. A player at one of them is sent wherever it is. Use it for a second network at home, or to try one player before turning the switch on. Do not put in the whole Tailscale range unless the machine is on Tailscale too: a phone away from home would be sent to a machine it cannot reach.

**Players that follow.** Being sent elsewhere for a film is something a player has to follow, and a player that does not will fail to play. Infuse 8.5 follows and seeks: on a Mac at home, and on a phone away from home, where the film came from the NAS over Tailscale. Infuse on an Apple TV has not been tried yet.

**What is playing, and from where,** is at the top of Activity while it plays: the title, and the machine it comes from or "Through this Spool".

**The address players use.** Players are sent to the address Spool itself uses for the helper. If they reach the NAS at a different one, give it under **Address players use**.

**If the helper is away,** nobody is sent to it and every film comes through Spool. A helper that stops in the middle of a film stops that film; starting it again plays it through Spool.

The helper sends no more than 48 parts of files at once, and turns away the next request at once, without keeping it waiting.

## Looking through its own folders

When a location is looked through or scanned, Spool has to know what is in each title's folder: the film or the episodes, their sizes, and the pictures, trailers and notes beside them. Across a share that is one request after another for every folder. A helper is asked instead, reads the folder on its own disks, and sends back what it found. Nothing has to be set for this; it happens for every folder a helper serves, when both are recent enough.

**What Spool concludes from it.** A file on record that the helper no longer finds is taken off the record, as it is when Spool reads the folder itself. So an answer has to be a whole one:

- A folder the helper could not read to the end is not answered at all, and Spool concludes nothing about it until the next scan.
- A shared folder that is empty, or is on another disk than it was when the helper started, is taken for a disk that has gone. So is a folder under a disk that was mounted inside a shared folder then and is not now. The helper says so, and no title there loses its files on record.
- When the helper was asked and could not say, Spool does not then read the same folder across the share. The share shows the same disks.

A scan says how many titles it left as they were for this reason, and names the title when there is one.

**Links.** The helper follows no link inside the folders it shares, so that it never tells of anything outside them. A folder with a link in it, or reached through one, is read by Spool across the share, as before. The helper checks for links by name as it goes; a link put in place of a folder at the very moment it is being read could still be followed, which matters only once a link is given to someone who is not trusted with the disks.

**A file that is new** still has its details read by Spool across the share. If the helper tells of a file a moment before the share shows it, it is put on record at the next scan, and a compact version waits for the file it was made from.

The helper looks through four folders at a time; more wait their turn.

## When it does not connect

**"The helper did not answer", and Spool runs on a Mac.** macOS asks before a program may reach other machines on the local network, and a Spool started as a service has nobody to ask: the request is refused without a word. Under System Settings, Privacy & Security, Local Network, turn on **spool**. The sign is that the helper answers `curl` from a shell on the same Mac and not Spool. Sign the program with a stable identity, as for [a removable disk](getting-started.md#running-it-as-a-service), or macOS takes each new build for a new program and the permission has to be given again.

**"Does not speak as this Spool does".** The two are too far apart in version to work together. Update both; see below.

**"From before it could hand films to players".** The helper is older than playing from it. It goes on downloading, filing and moving; update it to play from it.

**"Downloads into a folder this machine should see and cannot".** The helper is usable for moves, and downloads run on Spool's machine until the helper's downloads folder is shared and mounted there under the name it has in the container. Spool looks again by itself every half minute.

**"It does not see the folder where this machine does".** The two folders given for the helper do not name the same place, or a share is mounted on Spool's machine under a different name than in the container.

**The key is refused.** The key in Spool's settings is not the one the helper was started with. A helper answers nothing, not even what version it is, without its key.

## Keeping the two in step

Spool and a helper settle what they both understand when they connect. Each kind of thing a helper does, work and playing, is spoken in versions of its own, and the two use the newest they share. So a helper a release or two behind Spool goes on doing whatever both still understand, and the settings page says what that is. A helper from before this was so has the work and nothing else, as it always did.

Update the two together when you can. If they have no version of the work in common, Spool leaves the helper unused and says so in a notice.

The image is built and published by GitHub each time the main branch changes, as `ghcr.io/rewtraw/spool:latest`.

`deploy/helper.sh` updates a helper on TrueNAS SCALE over SSH: it waits for the image of the current commit to be built, asks the helper whether a restart would interrupt anything, pulls the image and restarts the app. `deploy/deploy.sh` runs it after updating Spool when `SPOOL_HELPER_HOST` is set. Its settings go in `deploy/local.env`, which is not tracked:

```sh
SPOOL_HELPER_HOST=root@nas            # where to reach the NAS over SSH
SPOOL_HELPER_URL=http://nas:7978      # where the helper answers
SPOOL_HELPER_KEY=<the helper's key>
```

Anywhere else, `docker compose pull && docker compose up -d` does the same.

A helper that restarts in the middle of things picks up its downloads where they stopped. A title it was moving stays where it was: the half-made copy is cleared away, and Spool reports that the move did not finish.

## Limits

- The Usenet speed limit applies to Spool's machine and to each helper separately, so together they may use more than the limit.
- A file that is new to Spool still has its details read across the share, a little of each.
- A film played from the NAS goes through Spool's machine for every player not named as one to send there.
- A folder with a link in it is read across the share, since the helper follows no link.
- A move between a helper's disks and anywhere else is Spool's own to carry out, across the share.
