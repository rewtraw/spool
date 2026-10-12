<script lang="ts">
  // The Players tab: serving the library to players, and the people who play: an account
  // for each, the players each is signed in on, and what they have watched.
  import { api } from '../../lib/api';
  import { act } from '../../lib/state.svelte';
  import { ago, duration } from '../../lib/format';

  let general = $state<any>(null);
  let door = $state<any>(null);
  let people = $state<any>({ accounts: [], recent: [] });
  let fresh = $state({ name: '', password: '' });
  // What is being typed for an account before it is saved, by the account's number.
  let edits = $state<Record<number, { name: string; password: string }>>({});

  async function loadPeople() {
    people = await api.get<any>('/accounts').catch(() => people);
    for (const a of people.accounts) edits[a.id] ??= { name: a.name, password: '' };
  }
  async function load() {
    general = await api.get('/settings/general');
    door = await api.get('/players').catch(() => null);
    await loadPeople();
  }
  load().catch(() => {});
  $effect(() => {
    const timer = setInterval(loadPeople, 5000);
    return () => clearInterval(timer);
  });

  async function saveDoor() {
    await act(() => api.put('/settings/general', { ...general, players_port: Number(general.players_port) }), 'Saved');
    // The players' door follows the settings within a few seconds.
    setTimeout(load, 4000);
  }
  async function add() {
    if ((await act(() => api.post('/accounts', fresh), `${fresh.name.trim()} can sign in now`)) === undefined) return;
    fresh = { name: '', password: '' };
    loadPeople();
  }
  async function change(a: any, what: Record<string, unknown>, said: string) {
    if ((await act(() => api.put(`/accounts/${a.id}`, what), said)) === undefined) return;
    edits[a.id].password = '';
    loadPeople();
  }
  async function remove(a: any) {
    if (!confirm(`Remove ${a.name}?\n\nWhat ${a.name} has watched and played is removed with the account, and its players are signed out. The library is not touched.`)) return;
    if ((await act(() => api.del(`/accounts/${a.id}`), `${a.name} was removed`)) !== undefined) loadPeople();
  }
  async function signOut(a: any, device: string) {
    if ((await act(() => api.del(`/accounts/${a.id}/devices/${device}`), 'Signed out')) !== undefined) loadPeople();
  }

  const summed = (a: any) =>
    a.totals.plays
      ? [`${duration(a.totals.seconds)} watched`, a.totals.seconds_lately > 0 ? `${duration(a.totals.seconds_lately)} this week` : '', `${a.totals.titles} ${a.totals.titles === 1 ? 'title' : 'titles'}`, `last played ${ago(a.totals.last_at)}`].filter(Boolean).join(' · ')
      : 'Nothing played yet';
</script>

{#if general}
  <form class="stack form" onsubmit={(e) => { e.preventDefault(); saveDoor(); }}>
    <fieldset class="card">
      <legend>Players</legend>
      <p class="muted">Spool can serve the library to a player on a television or a phone, such as Infuse on Apple TV. It answers as a Jellyfin server would: the player shows Spool's posters and details, plays each file as it is, and tells Spool what has been watched. Nothing is converted, so the player has to be able to play the files itself.</p>
      <label class="check"><input type="checkbox" bind:checked={general.players_enabled} /> Serve the library to players</label>
      {#if general.players_enabled}
        <label class="field narrow">Port<input class="input mono" type="number" min="1024" max="65535" bind:value={general.players_port} /><span class="hint">8096 is the one players expect.</span></label>
        <p class="muted">In the player, add a media server of the Jellyfin kind. On the same network it should appear by itself as <b>Spool</b>; otherwise give it the address <code>{location.hostname}:{general.players_port}</code>, and sign in as one of the people below.{#if door?.status} {door.status}{/if}</p>
      {/if}
      <div><button class="btn primary" type="submit">Save</button></div>
    </fieldset>
  </form>

  <div class="stack form">
    <fieldset class="card">
      <legend>People</legend>
      <p class="muted">Everyone who plays has an account of their own: a name and a password to sign a player in with. What each has watched, where they stopped and what comes next are theirs, and nobody else's change when they watch something.</p>
      {#each people.accounts as a (a.id)}
        <div class="place">
          <div class="row wrap">
            <b>{a.name}</b>
            <span class="small" class:warn={!a.enabled}>{[a.enabled ? '' : 'may not sign in', a.has_password ? '' : 'no password', a.devices.length ? `signed in on ${a.devices.length} ${a.devices.length === 1 ? 'player' : 'players'}` : 'not signed in anywhere'].filter(Boolean).join(' · ')}</span>
          </div>
          {#each a.playing as p}
            <div class="small"><span class="chip accent">Playing</span> <a href="#/title/{p.title_id}">{p.title}</a>{p.device ? ` on ${p.device}` : ''}, {p.from ? `from ${p.from}` : 'through this Spool'}</div>
          {/each}
          <div class="small muted">{summed(a)}</div>
          <details>
            <summary class="small">Name, password and players</summary>
            <div class="cols">
              <label class="field">Name<input class="input" bind:value={edits[a.id].name} autocomplete="off" /></label>
              <label class="field">New password<input class="input" type="password" bind:value={edits[a.id].password} autocomplete="new-password" placeholder={a.has_password ? 'Unchanged' : 'None'} /><span class="hint">A new password signs every one of {a.name}'s players out.</span></label>
            </div>
            <div class="row wrap">
              <button class="btn small" type="button" disabled={!edits[a.id].name.trim() || edits[a.id].name.trim() === a.name} onclick={() => change(a, { name: edits[a.id].name }, 'Renamed')}>Rename</button>
              <button class="btn small" type="button" disabled={!edits[a.id].password} onclick={() => change(a, { password: edits[a.id].password }, 'Password changed')}>Set password</button>
              {#if a.has_password}<button class="btn small ghost" type="button" onclick={() => confirm(`Let ${a.name} sign in with no password?\n\nAnyone who can reach Spool and knows the name could then play from the library.`) && change(a, { password: '' }, 'Password removed')}>Remove password</button>{/if}
              <button class="btn small ghost" type="button" onclick={() => change(a, { enabled: !a.enabled }, a.enabled ? `${a.name} may no longer sign in` : `${a.name} may sign in again`)}>{a.enabled ? 'Stop sign-in' : 'Allow sign-in'}</button>
              <span class="grow"></span>
              <button class="btn small ghost danger" type="button" onclick={() => remove(a)}>Remove {a.name}</button>
            </div>
            {#if a.devices.length}
              <div class="list">
                {#each a.devices as d (d.id)}
                  <div class="row wrap small">
                    <span><b>{d.name || 'A player'}</b>{d.client ? ` · ${d.client}` : ''}</span>
                    <span class="muted">{d.seen_at ? `seen ${ago(d.seen_at)}` : ''}</span>
                    <span class="grow"></span>
                    <button class="btn small ghost" type="button" onclick={() => signOut(a, d.id)}>Sign out</button>
                  </div>
                {/each}
              </div>
            {/if}
          </details>
        </div>
      {/each}
      {#if !people.accounts.length}<p class="small warn">Nobody can sign in yet. Add someone below.</p>{/if}
      <div class="place">
        <div class="cols">
          <label class="field">Name<input class="input" bind:value={fresh.name} autocomplete="off" placeholder="Someone new" /></label>
          <label class="field">Password<input class="input" type="password" bind:value={fresh.password} autocomplete="new-password" /><span class="hint">Without one, anyone who can reach Spool and knows the name can play from the library.</span></label>
        </div>
        <div><button class="btn" type="button" disabled={!fresh.name.trim()} onclick={add}>Add this person</button></div>
      </div>
    </fieldset>

    <fieldset class="card">
      <legend>Lately</legend>
      {#if people.recent.length}
        <div class="list">
          {#each people.recent as r (r.play.id)}
            <div class="row wrap small">
              <span class="muted">{ago(r.play.started_at)}</span>
              <span><b>{r.account}</b> played <a href="#/title/{r.play.title_id}">{r.play.title}</a></span>
              <span class="muted">{[r.play.device ? `on ${r.play.device}` : '', r.play.source ? `from ${r.play.source}` : 'through this Spool', `for ${duration(r.play.seconds)}`, r.play.finished ? 'to the end' : ''].filter(Boolean).join(' · ')}</span>
            </div>
          {/each}
        </div>
      {:else}
        <p class="muted">Nothing has been played since accounts were kept.</p>
      {/if}
    </fieldset>
  </div>
{/if}
