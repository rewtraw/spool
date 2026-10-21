# Clips

A clip is a few seconds or minutes cut from a film or an episode, to keep or to send to someone. Spool makes it as the one kind of video that plays everywhere it might go: a message, a chat, a phone, a browser.

## Making one

On a film's page, or a series' page, choose **Make a clip**.

- **Episode.** For a series, the episode to cut it from. The one you stopped part-way through is chosen to begin with.
- **Starts at.** Written as you would say it: `30m`, `1h2m5s`, `1:02:05`, `30:00`, or a number of seconds.
- **Seconds long.** Thirty to begin with, up to ten minutes. A clip that would run past the end stops at the end.
- **Up to where I stopped.** Where you stopped watching is offered as the start, so that the clip ends at the moment you stopped on.
- **Name.** If none is given it is called by the title and the time: "Westworld S01E07 at 30:00".

It is made in the background, one at a time, and appears under **Clips**. Half a minute of a 4K film takes a few seconds.

Anyone who can sign in can make clips and see them all. A clip is removed by whoever made it, or by someone who runs Spool.

## What a clip is

| | |
| --- | --- |
| Picture | H.264, no larger than 1080p, at a quality that holds up when it is watched full screen. |
| Colour | HDR (HDR10, HDR10+, HLG, and Dolby Vision over an HDR10 layer) is brought down to ordinary colour on the video hardware, so it does not look grey and washed out on a phone. |
| Sound | The soundtrack in the language chosen under Settings → General where the file has one, otherwise the file's own first choice, as stereo AAC. |
| File | MP4, laid out so that it starts playing before it has all arrived. |

Not in a clip: subtitles, other soundtracks, chapters, and Dolby Vision with nothing under it, which is refused with the reason.

A half-minute clip is usually between 10 and 30 MB.

## Sending one on

Each clip under **Clips** has:

- **Save.** Downloads the file. On an iPhone it goes to Downloads in the Files app, and can be sent from there to Messages, Photos or anywhere else.
- **Copy link.** A link that plays the clip for whoever opens it, without signing in. It works for anyone who can reach Spool: on your network, or on your Tailscale network. Each clip has a link of its own that cannot be guessed; removing the clip ends it.

Spool is reached over plain `http`, where browsers do not let a page open the share sheet. That is why there is a Save button and no Share button.

## From an assistant

An [assistant](assistants.md) can make one when asked: "clip Westworld S01E07 at 30m for 30 seconds". `make_clip` waits until the clip is made and gives the path of its file on the machine Spool runs on, so an assistant that can send files can send it on. `clips` lists them, and `delete_clip` removes one.

## Where they are kept

In a `clips` folder in Spool's data folder, apart from the library: one MP4 and one still for each. A clip outlives the film or episode it was cut from. A clip that was being made when Spool stopped is made again when it starts.
