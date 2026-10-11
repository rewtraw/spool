# Playing on a television

Spool can serve its library to a player on a television, a phone or a tablet. It answers the way a Jellyfin server does, so a player that knows how to talk to Jellyfin opens the library directly: there is no second server to run and no folders to share.

It is tested with [Infuse](https://firecore.com/infuse) on Apple TV and iPhone. Other players that speak to Jellyfin may work.

## Turning it on

Under Settings, General, Players:

1. Tick **Serve the library to players**.
2. Choose a **name** and a **password** for players to sign in with. These are separate from the web app's password, and open nothing but playback.
3. Leave the **port** at 8096 unless something else uses it. It is the one players expect.
4. Save.

In a container, publish the port as well (`8096:8096`) and give the player the machine's address. Being found without an address relies on a broadcast on the local network, which reaches a container only when it shares the host's network.

## Adding it in Infuse

1. In Infuse, add a share and choose **Jellyfin**.
2. On the same network, Spool appears in the list by itself as **Spool**. If it does not, enter the address of the machine Spool runs on and port 8096.
3. Sign in with the name and password from the settings.

If an Apple TV says an error occurred on a server it received through iCloud from another device, remove the server on the Apple TV and add it again there.

## What the player gets

- **The library as Spool knows it.** Posters, backdrops, cast, scores, collections, seasons and episodes, with search, recently added, continue watching and next up.
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

## Seeing what players are doing

Settings, General, Players shows whether Spool is listening and how many players are signed in. Changing the name or the password signs every player out.

## What is not there

- **Transcoding**, as above.
- **Several users.** There is one sign-in, and one record of what has been watched.
- **Live television, music and photos.**
- **A fast sync for Infuse's Library Mode.** Infuse's default, Direct Mode, is what Spool is tested with. See [Ideas](ideas.md).
