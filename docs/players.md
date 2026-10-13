# Playing on a television

Spool can serve its library to a player on a television, a phone or a tablet. It answers the way a Jellyfin server does, so a player that knows how to talk to Jellyfin opens the library directly: there is no second server to run and no folders to share.

It is tested with [Infuse](https://firecore.com/infuse) on Apple TV and iPhone. Other players that speak to Jellyfin may work.

## Turning it on

Under Settings, Players:

1. Tick **Serve the library to players**.
2. Leave the **port** at 8096 unless something else uses it. It is the one players expect.
3. Save.
4. Under **People**, add an account for each person who plays: a **name** and a **password** to sign a player in with. These are separate from the web app's password, and open nothing but playback.

In a container, publish the port as well (`8096:8096`) and give the player the machine's address. Being found without an address relies on a broadcast on the local network, which reaches a container only when it shares the host's network.

## Adding it in Infuse

1. In Infuse, add a share and choose **Jellyfin**.
2. On the same network, Spool appears in the list by itself as **Spool**. If it does not, enter the address of the machine Spool runs on and port 8096.
3. Sign in with the name and password of one of the people in the settings.

If an Apple TV says an error occurred on a server it received through iCloud from another device, remove the server on the Apple TV and add it again there.

## What the player gets

- **The library as Spool knows it.** Posters, backdrops, cast, scores, collections, seasons and episodes, with search, recently added, continue watching and next up.
- **Pictures for Movies and Shows.** The two views a player lists first are each given a picture made of the posters most lately added to them.
- **Collections.** A series of films, or an anime's series and films together, as [Spool groups them](collections.md).
- **Every file as it is.** Each version of a film is offered, with its subtitle files.
- **Your own artwork.** A poster or backdrop you keep in a title's folder is shown in place of the fetched one, and so are season posters and episode frames. See [Files Spool recognises](local-files.md#artwork).
- **Trailers and extras.** Trailers and other extras in a title's folder are offered to the player. Whether and where they are shown is up to the player.
- **What you have watched.** The player reports what was played and where you stopped, and Spool remembers it for next time and for other players.

Titles on a [location that cannot be reached](locations.md#when-a-location-cannot-be-reached) are left out until it comes back.

## Nothing is converted

Spool sends each file exactly as it is on disk. It does not transcode, so the player has to be able to play the file itself. Infuse plays nearly everything. For other players, a profile's "Prefer what an Apple TV plays without conversion" option steers downloads toward files that need no conversion.

Because nothing is converted, a file's full bitrate has to fit through the network between Spool and the player. On a home network that is rarely a concern. Away from home it is the upload speed of the connection Spool sits behind that sets the limit.

## Away from home

Spool's door for players is plain HTTP with a simple sign-in. It is made for a private network. **Do not forward the port from your router to the internet.**

To play away from home, put the player and the machine Spool runs on in the same private network:

- **[Tailscale](https://tailscale.com)** or another WireGuard network. Install it on both, and give the player the machine's name on that network and port 8096. This is the simplest way and works on Apple TV.
- **A VPN into your home network**, if your router offers one.

Finding Spool by itself works only on the local network. Over a private network, enter the address by hand.

## People

Each person who plays has an account of their own. What they have watched, where they stopped and what comes next are theirs: one person finishing a film does not mark it watched for another. Accounts are kept under Settings, Players.

- **A password** is the account's own. One with none lets anyone who can reach Spool and knows the name play from the library.
- **A new password** signs every one of that person's players out, and nobody else's.
- **Stop sign-in** turns an account away without removing it: its players are signed out, and what it has watched is kept for when it is allowed again.
- **Removing** an account removes what it watched and played with it. The library is not touched.
- **Players** signed in to an account are listed under it, each with what it called itself and when it was last seen, and can be signed out one at a time. A player not seen for ninety days is signed out by itself, so the list holds the players there are.
- **Choosing who.** A player that shows a list of people to sign in as is given the names of the accounts that may sign in. Whether a player shows one is the player's doing; one that does not asks for the name.

The account there was before there were several is the first one, with what had been watched and the players that were signed in.

## Compact versions only, away from home

Each account has **Away from home, give them compact versions only**. On, a player signed in to that account away from home is shown only the films and episodes that have a [compact version](compact.md), and is sent that version. At home it has everything.

- **What has no compact version is hidden** while away: not listed, not found by searching, and not played by asking for it by name. The settings page says how many films and episodes that is. Making compact versions of everything, under Settings, Compact versions, leaves nothing hidden.
- **The original is never sent.** A player that learned of the original at home and asks for it away is given the compact version.
- **At home** means on a network the machine running Spool is on. A player on Tailscale is placed by where it is really reached, so a phone on the home Wi-Fi is at home though it came by its Tailscale address, and one reached through a relay is away.
- **A player that comes through something else on Spool's machine**, such as a reverse proxy, looks as though it is at home.

## Bringing what was watched in Plex

With Plex set up under Settings, Plex, each account has **Bring watched from Plex**. Films and episodes Plex has as watched are marked watched for that account, and ones stopped part-way are given the place to carry on from.

- **It only adds.** Nothing watched here is unmarked, and a place kept here is not moved.
- **Whose history it is.** Plex answers for whoever its token belongs to, which is the person who set Plex up. Other people in a Plex Home are not read.
- **What it covers.** Titles that Spool tracks and Plex has. It can be run again at any time, and brings only what is new.

## Asking for something

The players' address, opened in a browser, is a page where the people who play ask for a film or a series the library has not got: `http://<the machine>:8096`. They choose who they are, give their password if the account has one, search by name and ask.

- **What is asked for waits** under **Asked for** on Home and in Settings, Players, with who asked. **Add it** adds it to the library and looks for it, as adding it under Discover would; **Not this time** turns it down.
- **Whoever asked sees how it stands** on the same page: asked, on its way, ready to play, or not this time. A series is ready when its first episode is, and says how many it has.
- **Added at once.** Each account has **Add what they ask for at once**. On, what that person asks for is added without waiting for you. It is off for a new account.
- **Two people asking for the same thing** are answered together.
- **What it opens.** The page signs in as a player does, so it is listed among the account's players. It can look titles up, ask, and take back its own asks. It cannot see the library's settings or anyone else's asks.

There is no message sent when something is ready: the page says so when it is next opened.

## Seeing what players are doing

- **Now.** What is playing is at the top of Activity, with who is playing it, on what, and whether it comes from a linked machine or through Spool. The same is under each person in Settings, Players.
- **Lately.** Settings, Players lists each sitting: who played what, on what, from where, for how long and whether to the end.
- **In all.** Each person has how long they have watched, how much of that in the last week, how many titles, and when they last played anything.

A sitting is counted from what the player reports as it plays. Time it says it was paused is not counted.

## What is not there

- **Transcoding**, as above.
- **Other limits on an account.** Apart from compact versions away from home, every account sees the whole library. Which locations a person may see, and how many may play at once, come with sharing between households; see [Ideas](ideas.md).
- **Live television, music and photos.**
- **A fast sync for Infuse's Library Mode.** Infuse's default, Direct Mode, is what Spool is tested with. See [Ideas](ideas.md).
