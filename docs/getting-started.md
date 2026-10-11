# Getting started

Spool runs on macOS and Linux, natively or in a container. It needs a Usenet provider, a Newznab indexer and somewhere to keep the library. It does not use torrents.

## Install it

### With Docker

```sh
git clone https://github.com/rewtraw/spool && cd spool
mkdir -p data                      # owned by the user in compose.yaml
$EDITOR compose.yaml               # set the user and the path to your media
docker compose up -d --build       # http://localhost:7979
```

The image carries the helper tools Spool calls. Everything Spool keeps lives in `/data`.

Mount the library and the downloads folder under one path. A finished download is then moved into the library, which takes no time, instead of being copied.

If Spool runs on one machine and the library is on a NAS, the NAS can run [a helper](helper.md): the same image, doing the downloading and filing on the NAS's own disks.

On **TrueNAS SCALE**, add it as a custom app with the same settings as [compose.yaml](https://github.com/rewtraw/spool/blob/main/compose.yaml): port 7979, a dataset for `/data`, your media dataset at `/media`, and the apps user (568) as the user.

### From source

You need Rust, [Bun](https://bun.sh), and four helper tools: `par2` for repair, `7zz` for extraction, and `ffprobe` and `ffmpeg` for reading media files and checking subtitles.

```sh
brew install par2 sevenzip ffmpeg                # macOS
sudo apt install par2 7zip ffmpeg                # Debian, Ubuntu

git clone https://github.com/rewtraw/spool && cd spool
(cd web && bun install && bun run build)         # the server embeds web/dist
cargo run --release -p spool -- serve            # http://localhost:7979
```

Debian's and Ubuntu's `7zip` cannot open RAR archives. Spool unpacks the common uncompressed kind itself, so most posts are unaffected. For the rest, install 7-Zip from [its own releases](https://github.com/ip7z/7zip/releases), as the container image does.

## The first settings

Open the web app and go to Settings. Three tabs need filling in before Spool can do anything.

1. **General.**
   - *Folders*: where movies and series live and where to download to. Name the disk that must be mounted, if the library is on one that can be unplugged.
   - *Connections*: a free [TMDB API key](https://www.themoviedb.org/settings/api), which Spool needs to look up movies. Set a password for the app if anyone else can reach the address.
2. **Usenet.** One or more providers. Priority 0 is asked first; higher numbers only fill in missing articles. The Test button signs in and reports what it found.
3. **Indexers.** One or more Newznab indexers, each with its address and API key.

The other tabs can wait:

| Tab | What it holds |
| --- | --- |
| Profiles | Which qualities a title may be downloaded in, where upgrades stop, and a size to aim for. |
| Naming | How imported files and folders are named. The formats use Radarr's and Sonarr's tokens. |
| Plex | What Plex has that Spool does not, and the other way round. See [Beside Plex](plex.md). |
| Disk space | What is using the disk and what could go. |
| Maintenance | The scheduled tasks, each with a Run now button, and the recycle folder. |

## Shadow mode, then active

Spool starts in **shadow mode**. It reads the indexer feeds and records what it would download and why, and changes nothing: no downloads, no moved files, no deletions. This is the time to add a few titles, open one, and read the verdicts it gives each release.

When the choices look right, switch to **active** under Settings, General, Mode, or from the command line:

```sh
spool mode active
```

Stop Radarr and Sonarr from managing the same folders first. Two programs filing into one library will fight over the same files.

Titles are added under **Discover**. One search looks for films and series together: the kind you chose is listed first and the other below it, so a film and the series of the same name are both in front of you.

If you already have a library, read [Your library and what Spool does to it](your-library.md) before going active.

## Coming from Radarr, Sonarr and SABnzbd

`spool migrate` reads their settings and brings over the catalog, profiles, indexers and Usenet servers. It can be run again safely.

```sh
spool migrate \
  --radarr-url http://localhost:7878 --radarr-key <key> \
  --sonarr-url http://localhost:8989 --sonarr-key <key> \
  --radarr-db radarr.db --sonarr-db sonarr.db \
  --sab-ini sabnzbd.ini
```

| Option | What it is for |
| --- | --- |
| `--radarr-url`, `--radarr-key`, `--sonarr-url`, `--sonarr-key` | Where the two apps answer, and their API keys. |
| `--radarr-db`, `--sonarr-db` | A copy of each app's database. Their APIs hide indexer keys and the Plex token; the databases have them. |
| `--sab-ini` | A copy of `sabnzbd.ini`, for the Usenet servers. |
| `--map FROM=TO` | Turns a path as a container saw it into the path on this machine. Repeat it for each mapping. |

Nothing on disk is moved. Two commands then check the result:

```sh
spool check-paths      # is every file where Spool's naming would put it?
spool shadow-report    # does Spool read past releases as Radarr and Sonarr did?
```

## Running it as a service

**Linux.** [deploy/spool.service](https://github.com/rewtraw/spool/blob/main/deploy/spool.service) is a systemd user unit. The steps to install it are at the top of the file.

**macOS.** `deploy/deploy.sh` builds Spool, installs it as a LaunchAgent on a Mac over SSH and restarts it. It reads its settings from the environment or from `deploy/local.env`:

```sh
SPOOL_HOST=my-mac deploy/deploy.sh
```

Run against a Spool that is already going, the script first asks it whether a restart would interrupt anything: a download being unpacked, an import, a title being moved, someone playing. If so it stops and says what; `SPOOL_FORCE=1` restarts anyway. Downloads still coming down are paused and resumed around the restart.

If the library is on a removable disk, set `SPOOL_SIGN_IDENTITY` to a stable code-signing identity. macOS ties the permission to read a removable disk to the program's signature, and the first read under launchd shows a consent prompt on that Mac's screen. Spool reports that it is waiting for the prompt instead of hanging.

## The command line

```sh
spool serve [--bind 0.0.0.0:7979]   # run the service
spool mode active|shadow            # start or stop making changes
spool migrate ...                   # import from Radarr, Sonarr and SABnzbd
spool check-paths                   # are existing files where Spool would put them?
spool shadow-report ...             # compare with Radarr's and Sonarr's history
spool check-subtitles VIDEO SUBS    # is this subtitle file in time? --fix corrects it
spool download x.nzb --out DIR      # run one NZB through the engine, outside the library
spool backup DEST                   # write a consistent copy of the database
```

## Where Spool keeps its data

| System | Folder |
| --- | --- |
| macOS | `~/Library/Application Support/Spool` |
| Linux | `~/.local/share/spool` |
| Container | `/data` |

`--data-dir` or `SPOOL_DATA_DIR` puts it elsewhere. The folder holds the database, the saved NZBs, cached artwork, the log and a nightly backup. The database contains indexer keys and Usenet passwords and is created readable only by its owner. Settings, General, Connections can name a second folder on another disk for a copy of the backup.

## Next

- [Your library and what Spool does to it](your-library.md)
- [A library in more than one place](locations.md)
- [A helper on the machine that holds the disks](helper.md)
- [Playing on a television](players.md)
