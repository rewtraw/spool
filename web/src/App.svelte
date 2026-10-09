<script lang="ts">
  import { onMount } from 'svelte';
  import { app, boot, route, go } from './lib/state.svelte';
  import { api } from './lib/api';
  import Home from './views/Home.svelte';
  import Library from './views/Library.svelte';
  import TitleView from './views/Title.svelte';
  import Discover from './views/Discover.svelte';
  import Activity from './views/Activity.svelte';
  import Archive from './views/Archive.svelte';
  import Settings from './views/Settings.svelte';

  const nav = [
    { id: 'home', label: 'Home', icon: 'M3 11.5 12 4l9 7.5V20a1 1 0 0 1-1 1h-5v-6H9v6H4a1 1 0 0 1-1-1z' },
    { id: 'library', label: 'Library', icon: 'M4 4h4v16H4zM10 4h4v16h-4zM16.5 5.2l3.8-1 3 14.6-3.8 1z' },
    { id: 'discover', label: 'Discover', icon: 'M11 4a7 7 0 1 0 4.4 12.5L20 21l1.4-1.4-4.6-4.6A7 7 0 0 0 11 4zm0 2a5 5 0 1 1 0 10 5 5 0 0 1 0-10z' },
    { id: 'activity', label: 'Activity', icon: 'M12 3v10.6l-3.3-3.3-1.4 1.4L12 16.4l4.7-4.7-1.4-1.4L12 13.6zM5 19h14v2H5z' },
    { id: 'archive', label: 'Archive', icon: 'M3 4h18v5H3zm2 7h14v9H5zm4 2.5v2h6v-2z' },
    { id: 'settings', label: 'Settings', icon: 'M4 6h10v2H4zm12 0h4v2h-4zM4 11h4v2H4zm6 0h10v2H10zM4 16h10v2H4zm12 0h4v2h-4zM14 4.5h2v5h-2zM8 9.5h2v5H8zM14 14.5h2v5h-2z' },
  ];

  let password = $state('');
  let loginError = $state('');

  onMount(boot);

  async function signIn(e: Event) {
    e.preventDefault();
    loginError = '';
    try {
      await api.post('/login', { password });
      password = '';
      await boot();
    } catch (err: any) {
      loginError = err.message;
    }
  }

  const active = $derived(route.view === 'title' ? 'library' : route.view);
  const busy = $derived(Object.values(app.jobs).filter((j) => !['completed', 'failed', 'paused'].includes(j.state)).length);

  function onKey(e: KeyboardEvent) {
    const t = e.target as HTMLElement;
    if (t && ['INPUT', 'TEXTAREA', 'SELECT'].includes(t.tagName)) return;
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    const map: Record<string, string> = { '1': 'home', '2': 'library', '3': 'discover', '4': 'activity', '5': 'archive', '6': 'settings' };
    if (map[e.key]) go(map[e.key]);
    if (e.key === '/') {
      e.preventDefault();
      if (!['library', 'discover', 'archive'].includes(route.view)) go('library');
      setTimeout(() => (document.querySelector('[data-search]') as HTMLInputElement | null)?.focus(), 30);
    }
  }
</script>

<svelte:window onkeydown={onKey} />

{#if app.signedOut}
  <main class="signin">
    <form class="card" onsubmit={signIn}>
      <div class="brand"><span class="mark"></span>Spool</div>
      <label class="field">Password<input class="input" type="password" bind:value={password} autocomplete="current-password" /></label>
      {#if loginError}<p class="error">{loginError}</p>{/if}
      <button class="btn primary" type="submit">Sign in</button>
    </form>
  </main>
{:else}
  <div class="shell">
    <nav aria-label="Main">
      <a class="brand" href="#/home"><span class="mark"></span><span class="word">Spool</span></a>
      <div class="links">
        {#each nav as n}
          <a href="#/{n.id}" class:active={active === n.id} aria-current={active === n.id ? 'page' : undefined}>
            <svg viewBox="0 0 24 24" width="20" height="20" aria-hidden="true"><path d={n.icon} fill="currentColor" /></svg>
            <span>{n.label}</span>
            {#if n.id === 'home' && app.attention > 0}<b class="badge">{app.attention}</b>{/if}
            {#if n.id === 'activity' && busy > 0}<b class="badge quiet">{busy}</b>{/if}
          </a>
        {/each}
      </div>
      {#if app.status}
        <div class="mode" class:shadow={app.status.mode === 'shadow'} title={app.status.mode === 'shadow' ? 'Watching and deciding only. Nothing is downloaded or moved.' : 'Downloading and importing.'}>
          <i></i>{app.status.mode === 'shadow' ? 'Shadow mode' : 'Active'}
        </div>
      {/if}
    </nav>

    <main>
      {#if !app.online}
        <div class="offline" role="status">Lost contact with Spool. Reconnecting…</div>
      {/if}
      {#if route.view === 'home'}<Home />
      {:else if route.view === 'library'}<Library />
      {:else if route.view === 'title'}{#key route.arg}<TitleView id={Number(route.arg)} />{/key}
      {:else if route.view === 'discover'}<Discover />
      {:else if route.view === 'activity'}<Activity />
      {:else if route.view === 'archive'}<Archive />
      {:else if route.view === 'settings'}<Settings />
      {:else}<div class="empty">Nothing here. <a href="#/home">Go home</a></div>{/if}
    </main>
  </div>
{/if}

<div class="toasts" aria-live="polite">
  {#each app.toasts as t (t.id)}
    <div class="toast" class:error={t.kind === 'error'}>{t.text}</div>
  {/each}
</div>

<style>
  .shell {
    display: grid;
    grid-template-columns: 212px minmax(0, 1fr);
    min-height: 100dvh;
  }
  nav {
    position: sticky;
    top: 0;
    height: 100dvh;
    display: flex;
    flex-direction: column;
    gap: 22px;
    padding: 22px 12px 18px;
    border-right: 1px solid var(--line);
    background: var(--surface);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 0 10px;
    font: 600 21px/1 var(--serif);
    letter-spacing: -0.01em;
  }
  .mark {
    width: 22px;
    height: 22px;
    border-radius: 50%;
    border: 3px solid var(--text);
    position: relative;
    flex: none;
  }
  .mark::after {
    content: '';
    position: absolute;
    inset: 4.5px;
    border-radius: 50%;
    background: var(--accent);
  }
  .links {
    display: grid;
    gap: 2px;
  }
  .links a {
    display: flex;
    align-items: center;
    gap: 11px;
    height: 38px;
    padding: 0 10px;
    border-radius: 8px;
    color: var(--muted);
    font-weight: 500;
  }
  .links a:hover {
    background: var(--raised);
    color: var(--text);
  }
  .links a.active {
    background: var(--accent-soft);
    color: var(--text);
  }
  .links a.active svg {
    color: var(--accent);
  }
  .badge {
    margin-left: auto;
    min-width: 20px;
    height: 20px;
    padding: 0 6px;
    border-radius: 999px;
    background: var(--warn);
    color: #1b1a17;
    font-size: 12px;
    display: grid;
    place-items: center;
  }
  .badge.quiet {
    background: var(--raised);
    color: var(--muted);
  }
  .mode {
    margin-top: auto;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 10px;
    font-size: 13px;
    color: var(--muted);
  }
  .mode i {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--ok);
  }
  .mode.shadow i {
    background: var(--warn);
  }
  main {
    padding: 30px clamp(16px, 3.5vw, 44px) 80px;
    min-width: 0;
  }
  .offline {
    margin-bottom: 16px;
    padding: 10px 14px;
    border-radius: 8px;
    background: color-mix(in srgb, var(--warn) 16%, transparent);
    color: var(--warn);
    font-size: 14px;
  }
  .signin {
    min-height: 100dvh;
    display: grid;
    place-items: center;
    padding: 20px;
  }
  .signin form {
    width: min(340px, 100%);
    padding: 26px;
    display: grid;
    gap: 16px;
  }
  .signin .brand {
    padding: 0;
  }
  .error {
    color: var(--bad);
    font-size: 14px;
  }
  .toasts {
    position: fixed;
    right: 18px;
    bottom: 18px;
    display: grid;
    gap: 8px;
    z-index: 50;
    max-width: min(420px, calc(100vw - 36px));
  }
  .toast {
    padding: 11px 14px;
    border-radius: 9px;
    background: var(--text);
    color: var(--bg);
    font-size: 14px;
    box-shadow: var(--shadow);
  }
  .toast.error {
    background: var(--bad);
    color: #fff;
  }

  @media (max-width: 760px) {
    .shell {
      grid-template-columns: minmax(0, 1fr);
    }
    nav {
      position: fixed;
      top: auto;
      bottom: 0;
      left: 0;
      right: 0;
      height: auto;
      z-index: 20;
      flex-direction: row;
      padding: 6px 6px calc(6px + env(safe-area-inset-bottom));
      border-right: none;
      border-top: 1px solid var(--line);
    }
    nav .brand,
    nav .mode {
      display: none;
    }
    .links {
      display: flex;
      width: 100%;
      justify-content: space-around;
    }
    .links a {
      flex-direction: column;
      gap: 3px;
      height: 48px;
      padding: 0 8px;
      font-size: 11px;
      justify-content: center;
      position: relative;
      flex: 1;
    }
    .badge {
      position: absolute;
      top: 2px;
      right: calc(50% - 22px);
      min-width: 16px;
      height: 16px;
      font-size: 10.5px;
      padding: 0 4px;
    }
    main {
      padding: 18px 14px calc(96px + env(safe-area-inset-bottom));
    }
    .toasts {
      bottom: calc(76px + env(safe-area-inset-bottom));
    }
  }
</style>
