<script lang="ts">
  // What is shared on the internet: channels' videos and clips, each with a link that plays it
  // for anyone until it is no longer shared. See shares.rs.
  import { api } from '../lib/api';
  import { act } from '../lib/state.svelte';
  import { ago, bytes } from '../lib/format';
  import { shareLink } from '../lib/save';

  type Share = { id: number; token: string; file_id: number | null; clip_id: number | null; title_id: number | null; name: string; from: string; description: string; state: 'making' | 'ready' | 'failed'; error: string; size: number; created_at: number; views: number; url: string | null };

  let shared = $state<{ shares: Share[]; address: string } | null>(null);
  let confirming = $state<number | null>(null);
  // The share being edited, and what its page is to say.
  let editing = $state<number | null>(null);
  let draft = $state({ name: '', from: '', description: '' });

  function edit(s: Share) {
    editing = s.id;
    draft = { name: s.name, from: s.from, description: s.description ?? '' };
  }
  async function saveEdit(s: Share) {
    if ((await act(() => api.patch(`/shares/${s.id}`, draft), 'Saved. Its page says so now.')) === undefined) return;
    editing = null;
    load();
  }

  async function load() {
    shared = await api.get<{ shares: Share[]; address: string }>('/shares').catch(() => shared ?? { shares: [], address: '' });
  }
  load();
  // One being got ready says when it is.
  $effect(() => {
    if (!shared?.shares.some((s) => s.state === 'making')) return;
    const timer = setInterval(load, 3000);
    return () => clearInterval(timer);
  });

  async function stop(s: Share) {
    confirming = null;
    await act(() => api.del(`/shares/${s.id}`), 'No longer shared: the link has ended');
    load();
  }

  const shortUrl = (url: string) => url.replace(/^https?:\/\//, '');
  const meta = (s: Share) => [s.clip_id ? 'Clip' : 'Video', bytes(s.size), `opened ${s.views} ${s.views === 1 ? 'time' : 'times'}`, `shared ${ago(s.created_at)}`].join(' · ');
</script>

<header class="head">
  <h1>Shared</h1>
</header>
<p class="muted lede">Videos and clips with a link that plays them for anyone on the internet, without signing in, until they are no longer shared. Share a channel's video from its page, or a clip from <a class="link" href="#/clips">Clips</a>.</p>

{#if shared === null}
  <div class="empty">Loading…</div>
{:else if !shared.address}
  <div class="card empty">Nothing can be shared until sharing has a public address. Set one under <a class="link" href="#/settings/people">Settings, People</a>.</div>
{:else if !shared.shares.length}
  <div class="card empty">Nothing is shared yet. Links are made at <b>{shortUrl(shared.address)}</b>.</div>
{:else}
  <div class="shares">
    {#each shared.shares as s (s.id)}
      <article class="card share">
        <div class="frame">
          {#if s.state === 'ready'}
            <!-- svelte-ignore a11y_media_has_caption -->
            <video src="/api/shares/{s.id}/video" poster="/api/shares/{s.id}/still" controls playsinline preload="none"></video>
          {:else if s.state === 'making'}
            <div class="placeholder"><span>Getting it ready to share…</span></div>
          {:else}
            <div class="placeholder bad"><span>It could not be got ready: {s.error}</span></div>
          {/if}
        </div>
        {#if editing === s.id}
          <form class="about edit" onsubmit={(e) => { e.preventDefault(); saveEdit(s); }}>
            <label class="small muted">Name <input class="input" bind:value={draft.name} required /></label>
            <label class="small muted">By <input class="input" bind:value={draft.from} placeholder="Who made it, or what it is from" /></label>
            <label class="small muted">About <textarea class="input" rows="3" bind:value={draft.description} placeholder="A few words, shown under it and where the link is sent"></textarea></label>
            <div class="row wrap">
              <button class="btn small primary" type="submit" disabled={!draft.name.trim()}>Save</button>
              <button class="btn small ghost" type="button" onclick={() => (editing = null)}>Cancel</button>
            </div>
          </form>
        {:else}
          <div class="about">
            <b class="name">{s.name}</b>
            <div class="muted small">{#if s.title_id}<a class="link" href="#/title/{s.title_id}">{s.from}</a>{:else}{s.from}{/if}</div>
            {#if s.description}<p class="description small">{s.description}</p>{/if}
            <div class="faint small">{meta(s)}</div>
            {#if s.url}<a class="url mono small truncate" href={s.url} target="_blank" rel="noreferrer">{shortUrl(s.url)}</a>{/if}
          </div>
        {/if}
        <div class="row wrap actions">
          {#if s.url && s.state === 'ready'}
            <button class="btn small primary" onclick={() => shareLink(s.url!, s.name)}>Send link</button>
            <a class="btn small ghost" href={s.url} target="_blank" rel="noreferrer">Open</a>
          {/if}
          {#if editing !== s.id}<button class="btn small ghost" onclick={() => edit(s)}>Edit</button>{/if}
          <span class="grow"></span>
          {#if confirming === s.id}
            <span class="small muted">End the link?</span>
            <button class="btn small danger" onclick={() => stop(s)}>Stop sharing</button>
            <button class="btn small ghost" onclick={() => (confirming = null)}>Keep</button>
          {:else}
            <button class="btn small ghost danger" onclick={() => (confirming = s.id)}>Stop sharing</button>
          {/if}
        </div>
      </article>
    {/each}
  </div>
{/if}

<style>
  .lede {
    margin: 0 0 18px;
    max-width: 70ch;
  }
  .shares {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(100%, 320px), 1fr));
    gap: 16px;
  }
  .share {
    padding: 0;
    overflow: hidden;
    display: flex;
    flex-direction: column;
  }
  .frame {
    aspect-ratio: 16 / 9;
    background: #000;
  }
  .frame video {
    display: block;
    width: 100%;
    height: 100%;
    object-fit: contain;
    background: #000;
  }
  .placeholder {
    height: 100%;
    display: flex;
    align-items: center;
    padding: 0 20px;
    color: #ddd;
    font-size: 13px;
  }
  .placeholder.bad {
    color: #f3b3ad;
  }
  .about {
    padding: 12px 14px 4px;
    display: grid;
    gap: 3px;
    min-width: 0;
  }
  .name {
    overflow-wrap: anywhere;
  }
  .edit {
    gap: 8px;
    padding-bottom: 8px;
  }
  .edit label {
    display: grid;
    gap: 3px;
  }
  .edit textarea {
    resize: vertical;
    font: inherit;
  }
  .description {
    margin: 4px 0 2px;
    color: var(--text);
    white-space: pre-line;
    overflow-wrap: anywhere;
  }
  .url {
    color: var(--muted);
    text-decoration: none;
    margin-top: 4px;
  }
  .url:hover {
    color: var(--text);
  }
  .actions {
    padding: 8px 14px 14px;
    margin-top: auto;
    align-items: center;
  }
</style>
