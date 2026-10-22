# Clips

A clip is a few seconds or minutes cut from a film or an episode, to keep or to send to someone. Spool makes it as the one kind of video that plays everywhere it might go: a message, a chat, a phone, a browser.

## Making one

On a film's page, or a series' page, choose **Make a clip**.

- **Episode.** For a series, the episode to cut it from. The one you stopped part-way through is chosen to begin with.
- **Starts at.** Written as you would say it: `30m`, `1h2m5s`, `1:02:05`, `30:00`, or a number of seconds.
- **Seconds long.** Thirty to begin with, up to ten minutes. A clip that would run past the end stops at the end.
- **Up to where I stopped.** Where you stopped watching is offered as the start, so that the clip ends at the moment you stopped on.
- **Subtitles.** Automatic draws them in when what is said is in another language than yours, as with anime in Japanese, and otherwise only the lines a film always shows, such as signs and words in another tongue. Or always, or none.
- **Name.** If none is given it is called by the title and the time: "Westworld S01E07 at 30:00".

It is made in the background, one at a time, and appears under **Clips**. Half a minute of a 4K film takes a few seconds.

Anyone who can sign in can make clips and see them all. A clip is removed by whoever made it, or by someone who runs Spool.

## Finding a moment by what is said

At the top of **Clips**, write what is said, or something like it, and choose **Find**: `wake up`, `the awakening`, `don't go to sleep`. Spool looks through what is said in every film and episode and lists the moments, each with what it is in, where, and a couple of lines either side, so you can tell which you meant. Look for several things at once by putting them apart with `;`.

It finds the words themselves, in any form of them (`awakening` finds "awakened"), and what is meant when it is put another way (`stay awake` finds "whatever you do, don't fall asleep"). Moments with the words themselves are marked and come first.

Tap a moment to see more of what is said around it, a minute either side and more with **Earlier** and **Later**; tap a line there to start or end its clip on that line. Tick the moments you want, choose how much before the first line and after the last each clip holds (none, unless you choose more) and whether it has subtitles (always, unless you choose otherwise), and choose **Make clips**. They are made one after another and appear below, each named by where it is from and the first words said.

### What can be searched

What is said is read from subtitles in your language (Settings → General): text inside the file, or a subtitle file beside it. Spool reads the whole library in the background, a file at a time, and pauses while anything is being played. A new film or episode is read soon after it arrives. Most files say where in them each subtitle is, and only those few kilobytes are read, in a second or two even from a share across the network; a file that does not say is read whole. Working out what the lines mean takes about half an hour for a whole library, on a Mac's graphics chip.

Some files have no subtitles that can be read: only pictures of them, as on a disc, or none at all. With an OpenSubtitles account set in Settings → General, Spool fetches subtitles for those, newest files first. It keeps only subtitles that prove to be in time with the film, judged by the subtitles already in it, even those that are pictures, or by its speech where there are none, puts them beside the video, where players find them too, and leaves a few of the day's allowance for looking by hand. A free account allows few a day; a VIP account, about a thousand. Files OpenSubtitles has nothing in time for are asked about again after a month.

Above the search, Spool says how much of the library can be searched yet, and what it is doing.

The first time, Spool fetches the small model it works out meaning with (all-MiniLM-L6-v2, about 90 MB) and keeps it in its data folder. Until it has it, searches go by the words alone. What is said is kept in `dialogue.db` beside Spool's own database, apart from it, as it can always be read again.

## What a clip is

| | |
| --- | --- |
| Picture | H.264, no larger than 1080p, at a quality that holds up when it is watched full screen. |
| Colour | HDR (HDR10, HDR10+, HLG, and Dolby Vision over an HDR10 layer) is brought down to ordinary colour on the video hardware, so it does not look grey and washed out on a phone. |
| Sound | The soundtrack in the language chosen under Settings → General where the file has one, otherwise the file's own first choice, as stereo AAC. |
| Subtitles | Drawn into the picture, so they show wherever it is played. Text subtitles, inside the file or in a file beside it, are drawn by Spool in white with a dark edge; the styling of anime subtitles is not kept. Subtitles that are pictures, as on a disc, are laid over as they are. |
| File | MP4, laid out so that it starts playing before it has all arrived. |

Not in a clip: other soundtracks, chapters, and Dolby Vision with nothing under it, which is refused with the reason. Text in scripts the font does not have, such as Chinese or Japanese, is not drawn.

A half-minute clip is usually between 10 and 30 MB.

## Sending one on

Each clip under **Clips** has:

- **Save.** On a phone with Spool open over `https`, opens the share sheet, where **Save Video** puts it straight into Photos. Elsewhere it downloads the file; on an iPhone that is Downloads in the Files app.
- **Save as GIF.** The same clip as a GIF, made the first time someone asks for it, which takes a few seconds, and kept. A clip of a few seconds comes out 480 pixels wide at 15 pictures a second; a longer one comes out smaller and with fewer, so that each stays near 10 MB, a file that can still be sent. It is saved the same way as the video, with **Save Image** in the share sheet.
- **Copy link.** A link that plays the clip for whoever opens it, without signing in. It works for anyone who can reach Spool: on your network, or on your Tailscale network. Each clip has a link of its own that cannot be guessed; removing the clip ends it.

Browsers open the share sheet only for a page reached over `https`. At `http://casper:7979` the buttons download instead. With [Tailscale Serve](https://tailscale.com/kb/1312/serve) in front of Spool, it is reached at an `https` address on your Tailscale network and the share sheet opens. If the sheet takes too long to come up after a large clip has been fetched, the button changes to **Save it now**; tap it again.

## From an assistant

An [assistant](assistants.md) can make one when asked: "clip Westworld S01E07 at 30m for 30 seconds". `make_clip` takes the same choices, subtitles among them, waits until the clip is made and gives the path of its file on the machine Spool runs on, so an assistant that can send files can send it on. `clips` lists them, and `delete_clip` removes one.

It can find them by what is said, too: "make clips of everything that says something like wake up". `search_dialogue` lists the moments with the lines around them, for you to choose from, and `make_clips` makes those you choose.

## Where they are kept

In a `clips` folder in Spool's data folder, apart from the library: one MP4 and one still for each, and a GIF once one has been asked for. A clip outlives the film or episode it was cut from. A clip that was being made when Spool stopped is made again when it starts.

## Of a video outside the library

`spool clip` cuts a clip of any video file, such as one downloaded with yt-dlp, with a subtitle file drawn in as Spool draws them: `spool clip talk.mp4 --from 0 --to 1:20 --subs talk.srt`. The subtitle file is timed against the whole video, and the clip comes out beside it as "talk clip.mp4" unless `--out` says otherwise. It needs no library or running Spool, only ffmpeg, and works with an ffmpeg that cannot draw text itself, as Homebrew's cannot. Correct a subtitle file's words before clipping; automatic captions from YouTube repeat each line and should be cut down to one of each first.
