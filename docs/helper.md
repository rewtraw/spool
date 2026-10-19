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

A helper is started with a list of folders and works only inside them. Every path it is given is checked against the list. A path that passes through a link is refused, even if the link sits inside one of the folders, because a link can lead outside them. A link inside a folder being moved isn't followed, and is left where it is.

| Setting | Meaning |
| --- | --- |
| `SPOOL_HELPER_KEY` | Sent by Spool with every request. At least 16 characters. Nothing is answered without it. |
| `SPOOL_HELPER_ROOTS` | Folders it may file into and move titles within, separated by colons. |
| `SPOOL_HELPER_READ_ROOTS` | Folders it may read and copy from. It never writes to them, moves anything out of them or removes anything from them. |
| `SPOOL_HELPER_DOWNLOADS` | The folder it downloads into. It clears away only finished downloads of its own there. |
| `SPOOL_HELPER_GRANTS` | What it does for whoever holds the key: any of `see`, `play`, `work` and `make`, separated by commas. Left out, all four. Without `work` it downloads, files and moves nothing; without `play` it hands no file to a player. `make` is making compact versions on video hardware, which a NAS has none of; a machine started with `make` alone needs no folders at all. See [More machines to make them](compact.md#more-machines-to-make-them). |

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

**The folders may be named as you like in the container.** Spool does not go by their names: it finds which of its own mounted shares each one is. See [Where its disks are](#3-tell-spool-about-the-helper).

Put the downloads folder on the same dataset as the library if you can: a finished download is then filed by renaming it. On another dataset it is copied, inside the NAS.

### 2. Let Spool's machine see the downloads folder

Spool reads what was downloaded before it files it: which file is the film, how long it runs, what is in it. So the helper's downloads folder has to be shared and mounted on Spool's machine, beside the library folders and under the name it has in the container (`/Volumes/nas/downloads` for the example above). Reading is enough; the share can be read-only.

### 3. Tell Spool about the helper

Under Settings, Library, choose **Link another Spool**:

| Field | What to put |
| --- | --- |
| Name | What to call it, such as the NAS's name. |
| Address | Where it answers, such as `http://nas:7978`. |
| Code | The code the helper gave, if it was started without a key: see [Linking with a code](#linking-with-a-code). Choose **Link** beside it. |
| Or its key | The key it was started with, if it was given one. |

A key that was typed in is not shown again once saved, and is not handed back to the browser.

**Where its disks are.** Spool and the helper see the same files under different names: a film is `/Volumes/nas/movies/Heat (1995)` to one and `/media/films/Heat (1995)` to the other. Spool finds which folder here is which folder there, and nothing is typed. The helper puts an empty file with a name nobody could guess in each folder it works in; Spool looks for those names among the shares mounted on its machine, and the folder with the mark in it is the same folder. The marks are taken away at once, and after half a minute if anything goes wrong. A folder the helper may only read is not written in, and is known by a file it holds.

- **A folder is found once its share is mounted on Spool's machine.** Until then the link says which folder has not been found, and Spool looks again twice a minute. **Look again** under the link looks at once.
- **Two folders here that answer to one there settle nothing,** and neither is taken: mount a share once.
- **What was found** is listed under the link, each folder with where it is on this machine. It is there to be read, not set.

Every location whose folders lie in one of them is served by the helper, and the settings page lists it under the helper.

### 4. Check

**Check this location**, on a location the helper serves, asks the helper whether it sees the same folder Spool does, and says whether Spool can see the helper's downloads. Anything that fails comes with what to do about it.

From then on, a download for a title on that location shows **On** and the helper's name in Activity, and nothing appears in the downloads folder on Spool's own machine. The Linked Spools section of the settings shows, for each helper, whether it answers, which locations it serves, how many downloads it is running and how much room it has for them.

### On TrueNAS SCALE

The helper is a custom app. Under Apps, Discover Apps, the menu beside Custom App offers **Install via YAML**: give it a name and paste the definition, with your datasets and a key of your own. From a shell on the NAS the same thing is one call:

```sh
midclt call -job app.create '{"custom_app": true, "app_name": "spool-helper", "custom_compose_config_string": "<the definition>"}'
```

Run it as the apps user (568), which is the user a share from TrueNAS usually maps everyone to, so that what the helper writes and what Spool writes across the share belong to the same owner. Share the helper's downloads folder over NFS, read-only, to the network Spool's machine is on.

## Being found

A helper says on the home network that it is there, every few seconds, and Spool lists the ones it hears under Settings, Library, as **Found on this network**, each with its address and whether it is waiting to be linked. **Link it** starts a link with the name and address filled in; the code or the key is still yours to give.

- **Hearing of one links nothing.** Anything on a network can say it is a Spool. What is heard is an address to try, and what shows that the machine at it is yours is that it holds the code it gave you, or the key you gave it. With a code, a machine that is not the one you meant learns nothing from the attempt. A key, typed in, is sent to whatever is at the address, so with something found prefer its code.
- **What it says** is its name, its port and whether it is waiting to be linked. No code, no key, no folders, nothing of the library. It says so to a group address of Spool's own (`239.255.77.79`, port 7977), which stays on the network it is said on, and not through Bonjour: on a NAS that belongs to a service that shares it with nobody.
- **Its name** is the machine's own, or `SPOOL_NAME` if the helper is given one.
- **In a container** the helper is heard only if it shares the host's network (`network_mode: host`): a container on its container system's own network neither hears nor is heard. Without that it works as before, and its address is typed.
- **On a Mac,** Spool needs the Local Network permission to listen, the same one it needs to reach a helper at all.
- **One that stops saying so** drops off the list in twenty seconds.

## Linking with a code

A helper can be started without a key. It then does nothing for anybody until it has been linked, and says in its log how:

```
this helper is not linked yet: in Spool, under Settings, Library, link another Spool and type this code  code="4821 0937"
```

Type that code into Spool, beside the helper's address, and choose **Link**. The two agree a key between themselves, the helper writes it down, and from then on it is linked as if the key had been typed into both. The code is never sent, and neither is the key: each side sends the other one message made from the code, and only two that hold the same code come out with the same key.

To choose the code yourself, start the helper with `SPOOL_LINK_CODE` set to eight digits or more, in place of `SPOOL_HELPER_KEY`. A code the helper makes up is a new one each time it starts.

- **A wrong code** links nothing, and the helper takes five of them. After that it takes no code at all for ten minutes, the right one included, and for twice as long each time it happens again, up to a day. Starting it again with the same code does not give five more; starting it with a different code does. Anything on the network can use the tries up without knowing the code, which is why they come back.
- **Once linked,** it takes no more codes. The key is in `link.json` in the helper's data folder, readable only by the helper. To link it afresh, stop it, delete that file, and start it again.
- **A helper started with a key** is linked by that key and gives no code. Nothing changes for one that is set up this way already.
- **What the code does not do.** It settles a key without the key being copied. What the two then say to each other is as open to the network they are on as it was before: on a home network that is the choice made by not using TLS, and a link over the internet wants a private network such as Tailscale under it.

## Playing from the helper

A film on the NAS is played, without a helper, by way of Spool's machine: the NAS sends it to Spool across the share, and Spool sends it on. A helper can hand the film to the player itself. Players still know one server, Spool, where they sign in, browse and report what was watched.

1. A player asks Spool for a film, as it always does.
2. If the film is on the helper's disks and the player is one you have named, Spool answers with the helper's address and a **ticket**.
3. The player takes the film from the helper.

The ticket names that one file, runs out after twelve hours, and is signed with a key made from the one Spool and the helper share. The helper hands a file to whoever comes with a ticket of Spool's and to nobody else; it has no sign-in and nothing to browse. It opens the file by going from the shared folder one name at a time and follows no link on the way, so a link placed in a shared folder leads nowhere. A library whose films are themselves links to somewhere else cannot be played from the helper; leave its players off the list below, and Spool goes on sending those films itself.

**Which players are sent.** None, until you say. Under Settings, Library, each linked machine has **Send players on the same network straight to it**. On, a player on the same network as the machine is sent to it; every other player, and every player while it is off, gets the film through Spool as before. The same network means the same stretch of addresses as the one the machine answers Spool from: the first three numbers of an address such as `192.168.1.20`.

**A player that reaches Spool over Tailscale** is seen at its Tailscale address, even at home. Spool asks Tailscale on its own machine where it is really talking to that player: when the two talk directly, the player is placed by that address, so it counts as on the network at home and not away. A player reached through one of Tailscale's relays is taken to be away and gets the film through Spool. If the linked machine is itself reached over Tailscale, every player on Tailscale counts as near it.

**A machine on both.** A linked machine that Spool reaches on the home network may be on your Tailscale as well. Spool looks for a device on its own Tailscale with the link's name, asks it what it is, and takes it for the same machine only if it answers as the link does. From then on a player away from home, on Tailscale, is sent to the machine's Tailscale address, and a player at home to its home address. The link's status says "also on Tailscale" when this has been found. The link's key is sent to that device to ask, so the name of a link should be the name of a machine of yours.

**Players that follow.** Being sent elsewhere for a film is something a player has to follow, and a player that does not will fail to play. Infuse 8.5 follows and seeks: on a Mac at home, and on a phone away from home, where the film came from the NAS over Tailscale. Infuse on an Apple TV has not been tried yet.

**What is playing, and from where,** is at the top of Activity while it plays: the title, and the machine it comes from or "Through this Spool".

**Asked before sending.** Before a player is sent to the helper, Spool asks it whether that file opens there now, and sends the film itself if it does not or if the helper takes more than two seconds to say. What it said stands for a minute, so a film being skipped about in is not asked after each time.

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

**"Downloads into a folder this machine should see and cannot".** The helper is usable for moves, and downloads run on Spool's machine until the helper's downloads folder is shared and mounted there under the name it has in the container. Spool looks again by itself every half minute.

**"It does not see the folder where this machine does".** What Spool found for this folder no longer holds: a share has been mounted somewhere else, or another one put in its place. Choose **Look again** under the link.

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
