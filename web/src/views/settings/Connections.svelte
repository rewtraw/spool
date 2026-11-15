<script lang="ts">
  // Connections: where releases come from and are downloaded, and the services Spool talks to.
  // Each card saves its own settings alone.
  import { api } from '../../lib/api';
  import { act, toast } from '../../lib/state.svelte';
  import { conf, loadSettings, saveSettings } from '../../lib/settings.svelte';
  import Secret from '../../components/Secret.svelte';
  import Usenet from './Usenet.svelte';
  import Indexers from './Indexers.svelte';

  // What is typed into the secrets, until saved. A secret left blank is kept; one cleared is
  // sent as null.
  const SECRETS = ['tmdb_api_key', 'plex_token', 'opensubtitles_api_key', 'opensubtitles_password', 'books.annas_key', 'books.florilegium_token'];
  let typed = $state<Record<string, string>>(Object.fromEntries(SECRETS.map((k) => [k, ''])));
  let cleared = $state<Record<string, boolean>>(Object.fromEntries(SECRETS.map((k) => [k, false])));
  const secret = (k: string) => (cleared[k] ? null : typed[k] || undefined);
  const set = (k: string) => !!conf.g?.secrets?.[k];
  function forget(keys: string[]) {
    for (const k of keys) (typed[k] = ''), (cleared[k] = false);
  }
  loadSettings();

  async function saveMeta() {
    if (await saveSettings({ tmdb_api_key: secret('tmdb_api_key') })) forget(['tmdb_api_key']);
  }
  async function savePlex() {
    const g = conf.g;
    if (await saveSettings({ plex_on: g.plex_on, plex_url: g.plex_url, plex_collections: g.plex_collections, plex_token: secret('plex_token') })) forget(['plex_token']);
  }
  async function saveSubtitles() {
    const g = conf.g;
    if (await saveSettings({ opensubtitles_username: g.opensubtitles_username, opensubtitles_api_key: secret('opensubtitles_api_key'), opensubtitles_password: secret('opensubtitles_password') })) forget(['opensubtitles_api_key', 'opensubtitles_password']);
  }
  async function saveBooks() {
    const b = conf.g.books;
    if (await saveSettings({ books: { languages: b.languages, florilegium_url: b.florilegium_url, annas_key: secret('books.annas_key'), florilegium_token: secret('books.florilegium_token') } })) forget(['books.annas_key', 'books.florilegium_token']);
  }
  async function saveSites() {
    await saveSettings({ video_cookies: conf.g.video_cookies.trim() });
  }

  // ---- AI access: the keys are asked for when wanted, never sent with the settings.
  const mcpUrl = `${location.origin}/mcp`;
  let readKey = $state(false);
  api.get<{ read_key: boolean }>('/settings/mcp').then((r) => (readKey = r.read_key)).catch(() => {});
  async function copyMcp(access: 'full' | 'read', fresh = false) {
    const r = await act(() => api.post<{ key: string }>('/settings/mcp', { access, new: fresh }));
    if (!r) return;
    if (access === 'read') readKey = true;
    const cmd = `claude mcp add --transport http --scope user spool ${mcpUrl} --header "Authorization: Bearer ${r.key}"`;
    try {
      await navigator.clipboard.writeText(cmd);
      toast('Copied. Paste it into a terminal.');
    } catch {
      prompt('Copy this command:', cmd);
    }
  }
  async function readOff() {
    if ((await act(() => api.post('/settings/mcp', { access: 'none' }), 'The read-only key is turned off')) !== undefined) readKey = false;
  }
</script>

<div class="stack form">
  <Usenet />
  <Indexers />

  {#if conf.g}
    {@const g = conf.g}
    <form onsubmit={(e) => { e.preventDefault(); saveMeta(); }}>
      <fieldset class="card">
        <legend>Film details</legend>
        <Secret label="TMDB API key" set={set('tmdb_api_key')} bind:value={typed.tmdb_api_key} bind:cleared={cleared.tmdb_api_key} hint="Needed to look up and add films. Free from themoviedb.org. Series come from TVmaze, which needs none." />
        <div><button class="btn primary">Save</button></div>
      </fieldset>
    </form>

    <form onsubmit={(e) => { e.preventDefault(); savePlex(); }}>
      <fieldset class="card">
        <legend>Plex</legend>
        <label class="check"><input type="checkbox" bind:checked={g.plex_on} /> Use Plex</label>
        <span class="hint">{g.plex_on ? 'Spool tells Plex when a folder changes, reads what Plex holds once an hour, and links each title to its page there.' : 'Off: nothing is asked of Plex and nothing from it is shown. Its address and token are kept.'}</span>
        <div class="cols">
          <label class="field">Address<input class="input mono" bind:value={g.plex_url} placeholder="http://127.0.0.1:32400" disabled={!g.plex_on} /></label>
          <Secret label="Token" set={set('plex_token')} bind:value={typed.plex_token} bind:cleared={cleared.plex_token} />
        </div>
        {#if g.plex_on && set('plex_token')}
          <label class="check"><input type="checkbox" bind:checked={g.plex_collections} /> Write Spool's collections into Plex</label>
          <span class="hint">Each title in a collection gets its name as a collection tag in Plex, beside any it has. Removing a tag in Plex is respected.</span>
        {/if}
        <div><button class="btn primary">Save</button></div>
      </fieldset>
    </form>

    <form onsubmit={(e) => { e.preventDefault(); saveSubtitles(); }}>
      <fieldset class="card">
        <legend>OpenSubtitles</legend>
        <p class="muted">For subtitles a release did not come with, in the languages chosen under Preferences.</p>
        <Secret label="API key" set={set('opensubtitles_api_key')} bind:value={typed.opensubtitles_api_key} bind:cleared={cleared.opensubtitles_api_key} hint="Free from opensubtitles.com, under API consumers in your profile." />
        <div class="cols">
          <label class="field">Username<input class="input" bind:value={g.opensubtitles_username} autocomplete="off" /></label>
          <Secret label="Password" set={set('opensubtitles_password')} bind:value={typed.opensubtitles_password} bind:cleared={cleared.opensubtitles_password} />
        </div>
        <div><button class="btn primary">Save</button></div>
      </fieldset>
    </form>

    <form onsubmit={(e) => { e.preventDefault(); saveBooks(); }}>
      <fieldset class="card">
        <legend>Books</legend>
        <Secret label="Anna's Archive key" set={set('books.annas_key')} bind:value={typed['books.annas_key']} bind:cleared={cleared['books.annas_key']} hint="From your account page on Anna's Archive, once you are a member." />
        <label class="field narrow">Languages<input class="input" bind:value={g.books.languages} placeholder="en" autocomplete="off" /><span class="hint">Two-letter codes: <code>en</code>, or <code>en, fr</code>. The book index keeps books in these alone.</span></label>
        <div class="cols">
          <label class="field">Florilegium<input class="input mono" bind:value={g.books.florilegium_url} placeholder="http://melchior:8790" autocomplete="off" /><span class="hint">Books go onto its shelf and into its library. Without it they are kept in Spool.</span></label>
          <Secret label="Its token" set={set('books.florilegium_token')} bind:value={typed['books.florilegium_token']} bind:cleared={cleared['books.florilegium_token']} />
        </div>
        <div><button class="btn primary">Save</button></div>
      </fieldset>
    </form>

    <form onsubmit={(e) => { e.preventDefault(); saveSites(); }}>
      <fieldset class="card">
        <legend>Signing in to sites</legend>
        <p class="muted">Instagram, and some posts on X, show only to someone signed in. Videos are fetched with the sign-in of a browser on this machine, or a cookies.txt file exported from one.</p>
        <label class="field">Browser, or the full path of a cookies.txt file
          <input class="input mono" list="browsers" placeholder="Nowhere" bind:value={g.video_cookies} />
          <datalist id="browsers"><option value="safari"></option><option value="chrome"></option><option value="firefox"></option><option value="brave"></option><option value="edge"></option></datalist>
        </label>
        <div><button class="btn primary">Save</button></div>
      </fieldset>
    </form>
  {/if}

  <fieldset class="card">
    <legend>AI access</legend>
    <p class="muted">Spool speaks the Model Context Protocol at <code>{mcpUrl}</code>, so an AI session can look things up, add titles, manage downloads and read the log. Deleting files always needs an explicit confirmation.</p>
    <div class="row wrap">
      <button class="btn" type="button" onclick={() => copyMcp('full')}>Copy the Claude Code command</button>
      <button class="btn ghost" type="button" onclick={() => copyMcp('read')}>Copy it with a read-only key</button>
      {#if readKey}<button class="btn ghost" type="button" onclick={() => copyMcp('read', true)}>Make a new read-only key</button><button class="btn ghost danger" type="button" onclick={readOff}>Turn the read-only key off</button>{/if}
    </div>
    <span class="hint">The read-only key can look but change nothing. Making a new one stops the old one working.</span>
  </fieldset>
</div>
