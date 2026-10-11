# Collections and anime

A collection is a set of titles that belong together: the films of one series of films, or an anime's series, films and specials. Spool builds them by itself and shows them in its library, to players, and, if you ask, in Plex.

## Where they come from

- **Series of films.** TMDB says which collection a film belongs to. Two or more of its films in your library make the collection.
- **Anime.** [AniList](https://anilist.co) keeps each series, film, OVA and special as a work of its own and records what each is to the others: sequel, side story, recap, alternative telling, spin-off. Two or more titles in your library that those relations tie together make a collection, named for the work the story began with.

A title is in one collection at most. An anime's collection takes it before a film collection does, because it holds the series as well as the films.

Collections that mix live-action films and series, such as a franchise with both, are not built: neither source ties those together.

## In the library

**Collections** in the library's toolbar shows each collection as one tile in place of its titles. The tile opens the collection's page:

- its titles in the order they came out, each marked with what it is when it is not simply part of the story: **Recap**, **Side story**, **Alternative telling** or **Spin-off**;
- for anime, the works of the same story that are **not in the library**, each with an Add button where Spool knows the ids to add it by.

A title's own page says which collection it is part of.

## On a television

| Where | What you get |
| --- | --- |
| Players that speak to Spool as a Jellyfin server, such as Infuse | Each collection is offered as a Jellyfin collection, films and series together, in the order they came out. How a player shows a collection that holds both films and series is up to the player. |
| Plex | Off unless you turn it on: Settings, General, Connections, **Write Spool's collections into Plex**. Each title in a collection gets the collection's name as a collection tag, beside any tags it already has. Plex keeps collections per library, so one that holds films and series appears in both libraries under the same name. A tag you later remove in Plex is not put back. |
| A Jellyfin or Emby server reading the same folders | Nothing is written for them. They build film collections from TMDB themselves. |

## What else anime gets

Anime is filed awkwardly by the film and series sources, and the people who post it follow anime databases. Knowing a title's AniList entry lets Spool:

- **Know a "film" that is really episodes.** An OVA of six episodes is one film to TMDB and is posted as six files. AniList says so outright, so Spool turns single episodes down for the film and offers [Switch to series](your-library.md#what-can-go-wrong) before anything is downloaded.
- **Recognise the name a release carries.** Fan releases often use the romanised Japanese name. Spool adds the first entry's English and romanised names to the names it accepts for the title.

## How a title is recognised as anime

Never by guessing at names. A community-maintained list, [Fribb/anime-lists](https://github.com/Fribb/anime-lists), ties AniList's ids to IMDb's and TVDB's. A film is found by its IMDb id and a series by its TVDB id, each of its seasons being an AniList entry of its own. A title the list does not know is not treated as anime.

## What Spool asks for, and how often

- **The id list** is fetched once a week and kept beside Spool's database.
- **AniList** needs no account and no key. It allows thirty requests a minute; Spool asks for ten titles in one request, with a pause between requests, and asks about each title again after a month. A title is asked about when it is added, and the rest of the library by the **Read AniList** task, once a day; Settings, Maintenance can run it now.
- What AniList says is kept in the database, so collections are worked out without asking anyone.

## Limits

- Titles are tied together only through works AniList relates to them directly, or to a work beside them. Two seasons of a very long story with several missing seasons between them may not be joined until one of the missing ones is added.
- Relations are AniList's. A cross-over or a shared character does not join two stories; a spin-off does.
- Missing works are listed for anime only. For a series of films, Spool shows what you have.
