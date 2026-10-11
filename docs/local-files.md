# Files Spool recognises in a title's folder

Beside the film or the episodes, a title's folder often holds extras, artwork, theme music and a media server's notes. Spool reads these under the names Plex, Jellyfin, Emby and Kodi use. It never writes or deletes them; the one thing it does is rename artwork and notes that were named after a film's file when an upgrade gives that file a new name.

Names are matched without regard to capitals.

## Extras

A video is an extra, not the film or an episode, when it is in a folder with one of these names, anywhere under the title's folder:

| Folder | Kind |
| --- | --- |
| `Trailers` | Trailer |
| `Behind The Scenes` | Behind the scenes |
| `Deleted Scenes` | Deleted scene |
| `Featurettes` | Featurette |
| `Interviews` | Interview |
| `Scenes` | Scene |
| `Shorts` | Short |
| `Clips` | Clip |
| `Other`, `Extras` | Other |
| `Samples` | Sample |
| `Backdrops` | Theme video |

or when its name ends with one of these markers:

| Ending | Kind |
| --- | --- |
| `-trailer`, `.trailer`, `_trailer`, ` trailer`, or the whole name `trailer` | Trailer |
| `-sample`, `.sample`, `_sample`, ` sample`, or the whole name `sample` | Sample |
| `-behindthescenes` | Behind the scenes |
| `-deleted`, `-deletedscene` | Deleted scene |
| `-featurette` | Featurette |
| `-interview` | Interview |
| `-scene` | Scene |
| `-short` | Short |
| `-clip` | Clip |
| `-other`, `-extra` | Other |

A release name ends with the group that made it, and groups have all sorts of names. So a file whose name carries a quality (`1080p`, `BluRay`, `WEB-DL`) is not taken for an extra because it ends in `-SCENE` or `-OTHER`; only `trailer` and `sample` still mark such a name.

## The film itself

- **Versions.** Several films' worth of video in one film folder are one film and versions of it kept beside it: `Film (1999) - 2160p.mkv`, `Film (1999) - Directors Cut.mkv`. The largest is the main file, which an upgrade replaces; the others are left alone.
- **Parts.** Files that end in `cd1`, `disc 2`, `part-1`, `pt2`, `dvd1`, `disk1`, or the same with a letter from `a` to `d`, are the parts of one film and are replaced together.
- **Ids in a folder's name.** `{tmdb-603}`, `{imdb-tt0133093}`, `{tvdb-79126}` as Plex writes them, and `[tmdbid-603]`, `[imdbid-tt0133093]`, `[tvdbid-79126]` as Jellyfin and Emby do.

## Artwork

Pictures may be `.jpg`, `.jpeg`, `.png`, `.webp` or `.tbn`. A name may stand alone (`poster.jpg`) or follow the film's file name and a hyphen (`Film (1999)-poster.jpg`), and may end in a number where there are several (`fanart-2.jpg`, `backdrop3.jpg`).

| Names | What it is |
| --- | --- |
| `poster`, `folder`, `cover`, `default`, `movie`, `show`, `keyart`, or a picture with exactly the film's file name | Poster |
| `fanart`, `backdrop`, `background`, `art`, anything in an `extrafanart` folder | Backdrop |
| `banner` | Banner |
| `logo`, `clearlogo` | Logo |
| `landscape`, `thumb` | Thumbnail |
| `clearart` | Clear art |
| `disc`, `discart`, `cdart` | Disc art |
| `square`, `squareArt`, `backgroundSquare` | Square art |

For series:

| Names | What it is |
| --- | --- |
| `Season01.jpg`, `Season01b.jpg`, `season01-poster.jpg`, or `poster.jpg` inside the season's folder | That season's poster |
| `season01-banner.jpg`, `season01-fanart.jpg` | That season's banner or backdrop |
| `season-specials-poster.jpg`, or `poster.jpg` inside `Specials` | The specials' poster |
| A picture with an episode's file name, or that name and `-thumb` | That episode's frame |

A folder's own poster and backdrop are shown in place of the ones from TMDB or TVmaze, in the app and to players. So are a season's poster and an episode's frame. The other kinds are recorded and listed, and not yet shown anywhere.

## Notes

| File | Written by |
| --- | --- |
| `movie.nfo`, or an `.nfo` with the film's file name | Kodi, Jellyfin, Emby |
| `tvshow.nfo` | Kodi, Jellyfin, Emby |
| `.plexmatch` at the top of the folder | Plex |

Spool reads the ids from them (`<uniqueid type="tmdb">`, `<tmdbid>`, `<imdbid>`, `<tvdbid>`, an IMDb id in `<id>`, or a link to the title's page on IMDb or TMDB; `tmdbid:`, `tvdbid:`, `imdbid:` lines in `.plexmatch`), with the title and the year. They are how a folder with an unhelpful name is recognised when a location is brought in. A release's own `.nfo`, named after nothing in the folder, is ignored.

## Theme music

`theme.mp3` (or `.flac`, `.m4a`, `.ogg`, `.wav`, `.aac`, `.opus`), and anything in a `theme-music` folder.

## Where these come from

- Jellyfin: [Movies](https://jellyfin.org/docs/general/server/media/movies/) and [Shows](https://jellyfin.org/docs/general/server/media/shows/)
- Plex: [Local files for trailers and extras](https://support.plex.tv/articles/local-files-for-trailers-and-extras/), [Local media assets for movies](https://support.plex.tv/articles/200220677-local-media-assets-movies/) and [for TV shows](https://support.plex.tv/articles/200220717-local-media-assets-tv-shows/)
- Kodi: the NFO and artwork pages of its wiki
