# A helper on the machine that holds the disks

Spool runs on one machine and may keep part of its library on another, such as a NAS, which it reaches over a [network share](locations.md). Without help, everything it does to files there crosses the network: a download arrives at Spool's machine and is copied over, and a title moved between two of the NAS's own disks is read to Spool's machine and written back.

A helper is a small Spool that runs on the NAS itself and does only the file work, on the NAS's own disks. The Spool you already have stays in charge: it holds the catalogue, makes every choice, and serves the web app and players. It tells the helper what to fetch and what goes where.

This part of Spool is new. It is tested end to end, with a helper and a Spool in one test; it has had little use yet on a real network.

## What the helper does, and what it leaves alone

| Job | Without a helper | With one |
| --- | --- | --- |
| Downloading for a title kept on the NAS | Fetched by Spool's machine, then copied across the network | Fetched from Usenet by the NAS, onto its own disks |
| Repairing and unpacking | On Spool's machine | On the NAS |
| Filing into the library | A copy across the network | A rename, or a copy from one of the NAS's disks to another |
| Moving a title between two folders on the NAS | Read to Spool's machine and written back | Disk to disk inside the NAS |

Still done by Spool across the share, as before:

- reading folders and each file's details, when a location is looked through or scanned;
- playing, subtitles, recycling and deleting;
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
      SPOOL_HELPER_ROOTS: "/mnt/tank/movies:/mnt/tank/tv"
      SPOOL_HELPER_READ_ROOTS: "/mnt/archive/movies"
      SPOOL_HELPER_DOWNLOADS: "/mnt/tank/downloads/spool"
    volumes:
      - /mnt/tank/apps/spool-helper:/data
      - /mnt/tank/movies:/mnt/tank/movies
      - /mnt/tank/tv:/mnt/tank/tv
      - /mnt/tank/downloads:/mnt/tank/downloads
      - /mnt/archive/movies:/mnt/archive/movies:ro
```

Mount each folder at the same path inside the container as outside. The paths the helper reports are then the paths on the NAS, which keeps the next step simple.

Put the downloads folder on the same dataset as the library if you can: a finished download is then filed by renaming it. On another dataset it is copied, inside the NAS.

### 2. Let Spool's machine see the downloads folder

Spool reads what was downloaded before it files it: which file is the film, how long it runs, what is in it. So the helper's downloads folder has to be shared and mounted on Spool's machine, beside the library folders. Reading is enough; the share can be read-only.

### 3. Tell Spool about the helper

Under Settings, General, Helpers, choose **Add a helper**:

| Field | What to put |
| --- | --- |
| Name | What to call it, such as the NAS's name. |
| Address | Where it answers, such as `http://nas:7978`. |
| Key | The key it was started with. |
| The other machine's disks, as this machine sees them | The folder the shares are mounted under here, such as `/Volumes/nas`. |
| The same folder, as the helper sees it | Where the NAS keeps them, such as `/mnt`. |

The two folders must name the same place. A path is turned from one machine's to the other's by swapping them at its start: with the example above, `/Volumes/nas/tank/movies/Heat (1995)` here is `/mnt/tank/movies/Heat (1995)` to the helper.

There is nothing to set on a location. Every location whose folders lie under the first folder is served by the helper, and the settings page says which those are.

### 4. Check

**Check this location**, on a location the helper serves, asks the helper whether it sees the same folder Spool does, and says whether Spool can see the helper's downloads. Anything that fails comes with what to do about it.

From then on, a download for a title on that location shows **On** and the helper's name in Activity.

## Keeping the two in step

The helper and Spool must be the same version. Spool asks when it connects, and if the helper does not speak as it does, it leaves the helper unused and says so in a notice, until the two are updated together.

`deploy/helper.sh` updates a helper on TrueNAS SCALE over SSH: it waits for the image of the current commit to be built, asks the helper whether a restart would interrupt anything, pulls the image and restarts the app. `deploy/deploy.sh` runs it after updating Spool when `SPOOL_HELPER_HOST` is set.

A helper that restarts in the middle of things picks up its downloads where they stopped. A title it was moving stays where it was: the half-made copy is cleared away, and Spool reports that the move did not finish.

## Limits

- The Usenet speed limit applies to Spool's machine and to each helper separately, so together they may use more than the limit.
- The folders a location is looked through or scanned from are still read across the share.
- A film played from the NAS still goes through Spool's machine to the player.
- A move between a helper's disks and anywhere else is Spool's own to carry out, across the share.
