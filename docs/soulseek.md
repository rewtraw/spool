# Soulseek

Albums are looked for on the Usenet indexers first. When nothing there would do, Spool looks on Soulseek, a network of people sharing their music folders, and downloads the best folder it finds from one person. The folder is checked and filed like any download: it must have the album's tracks, be all FLAC or all 320k, and its tags must not name another album.

Spool shares your music folders back, two uploads at a time. Soulseek expects that: many people send nothing to someone who shares nothing.

## Setting it up

Under Settings, Connections, Soulseek, give a username and a password. There is no sign-up: the first time a name nobody has taken signs in, the server makes the account with that password. A forgotten password cannot be reset, so keep it somewhere.

Forward the port shown there, 2234 unless you change it, on your router to the machine Spool runs on, for TCP. Without it Spool still works, but it can only reach people whose own port is open.

## What is chosen

Among the folders found, the one with the best copy is taken: FLAC before 320k. Between equal copies, someone who would send it at once comes first, then a short queue, then speed. Someone whose queue was longer than 50 is passed over, and no more than two albums are asked of one person at once. A folder's path must name the album: as `Artist - Album`, as the album's own folder inside one named for the artist, or, for an album filed its own way as a series often is, with every word of the artist's name and the album's in it and exactly the album's tracks.

The other folders that would do are kept, and tried in turn when one does not, without searching again. A folder is given up, and the next tried, when its owner:

- has not said where it is in their queue within fifteen minutes,
- has it further back than 20th in their queue,
- has not begun sending it within an hour, or
- has sent nothing for an hour.

One its owner refuses is passed over from then on. Time Spool is not signed in does not count against anyone.

## How often it searches

Searches Spool makes by itself are wishlist searches, which the server allows one of every twelve minutes or so; albums take turns, and each is looked for on Soulseek at most once a day. Look now, on an album's page, searches at once. The indexers are still asked every six hours.

## What others can see

Others see your music folders as `Music\Artist - Album (Year) [FLAC]\01 Track.flac`, and nothing outside them. The person who sends you a file, and those you send to, see your IP address.

## The client

Spool speaks the protocol itself, with no other program to install. It follows the description the Nicotine+ project keeps, signs in as a client of its own, and leaves out chat rooms, private messages and browsing other people's shares.
