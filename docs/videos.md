# Videos from YouTube and other sites, and lists that add themselves

## Videos by their links

Paste a link under **Discover → Videos**: a video on YouTube, a post on X or Instagram, a clip on TikTok or Vimeo, or a page on any of the well over a thousand other sites [yt-dlp](https://github.com/yt-dlp/yt-dlp) reads. Pasting a link into the search on any other Discover tab takes you there too.

Each video is fetched at the best quality the site has, with its picture and its captions, and filed under the channel or account that posted it:

```
Videos/
  Some Channel/
    poster.jpg
    Season 2025/
      Some Channel - 2025-11-14 - What it is called [dQw4w9WgXcQ].mkv
      Some Channel - 2025-11-14 - What it is called [dQw4w9WgXcQ].en.srt
      Some Channel - 2025-11-14 - What it is called [dQw4w9WgXcQ]-thumb.jpg
```

Spool keeps a channel as a series of its videos: a season for each year, an episode for each video, numbered by the day it was posted (the second video of 14 November is 111402). So everything Spool does with an episode it does with a video: players list and play them, subtitles show, [clips](clips.md) can be cut from them and what is said in them can be searched for, watched state is kept, and [compact versions](compact.md) can be made. Channels have a tab of their own in the Library.

A channel is never searched for on the indexers, and Spool never asks TVmaze or TMDB about it. The id in brackets at the end of each name ties the file to its video when the folder is read again.

### Captions

Captions people wrote come before the ones a site makes from the speech, and of a site's own, the original before a translation. A site's own captions roll: each line is shown again with the next added below it, a word at a time. Spool makes them into one subtitle for each line, so a line is read once, shown once and found once. Only the languages wanted for subtitles (Settings → General) are kept, or the household's language when none are set. OpenSubtitles is not asked about these videos: it has nothing for them.

### A channel, an account or a playlist

A link to a channel, an account or a playlist brings its 10 newest videos, all of them, or none, as you choose. **Follow** keeps fetching what it posts: a followed channel is looked at every hour, and its new videos are fetched. What it had when it was followed and was not asked for is left alone. A channel can be followed or let go from its page, from the list of channels under Discover → Videos, or by an assistant.

Following works best for YouTube. X and Instagram list an account's posts only to someone signed in, and change how they do it often; a single post from either is fetched without trouble.

### Signing in

Instagram, and some posts on X, are shown only to someone signed in. Under **Discover → Videos → Signing in to sites**, name a browser on the machine Spool runs on (`safari`, `chrome`, `firefox`) to borrow its sign-in, after signing in to the site there, or give the full path of a cookies.txt file exported from a browser.

### What it needs

- **yt-dlp**, on the machine Spool runs on: `brew install yt-dlp`, or `pip install yt-dlp`. Spool looks for it where package managers put it; `SPOOL_YT_DLP` names another. Keep it up to date: sites change, and yt-dlp follows them.
- **ffmpeg**, to join a video's picture and sound, and **deno** or **node**, which yt-dlp needs to read YouTube. Spool runs yt-dlp with a search path that includes Homebrew's folders, so a service started by the system finds them.
- **A videos folder**, set on a location under Settings → Storage, on a location Spool may change.

Two videos are fetched at a time, into `spool-videos` beside the downloads folder, and filed through the same journal as a film, so a restart part-way through finishes the filing. Each one shows on the Activity page while it comes.

## Lists that add themselves

Paste a Letterboxd list under **Discover → Lists**: a critic's list, a festival's, the Criterion Collection, "1001 Movies You Must See", anyone's watchlist. Every film on it the library lacks is added with the profile you choose, and looked for. The list is read again every six hours, and films put on it since are added.

A film added from a list and later removed from the library is not added again. Films the list has that TMDB lists as series are left out, as are the few Letterboxd cannot tie to TMDB; the list says how many.

Letterboxd has no API for this, so Spool reads its pages, one a second: each page of the list, then each film's own page once for its TMDB id, which is remembered for every list. A long list takes some minutes the first time and a few page reads after that. A private list cannot be read.

## Assistants

| Tool | What it does |
| --- | --- |
| `get_video` | Fetch a video by its link, or a channel's newest, all or none, and follow it. |
| `videos` | What is being fetched and how far it has got, what failed and why, and the channels. |
| `follow_channel` | Follow a channel, or stop. |
| `lists`, `follow_list` | The Letterboxd lists followed, and following another. |
