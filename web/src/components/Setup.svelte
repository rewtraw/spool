<script lang="ts">
  // What Spool still needs before it can work, each with where to give it. Shown until it is
  // all done; the optional ones are listed but do not keep it showing.
  import { api } from '../lib/api';
  import { app } from '../lib/state.svelte';

  let setup = $state<any>(null);
  $effect(() => {
    app.tick;
    api.get('/setup').then((s) => (setup = s)).catch(() => {});
  });
</script>

{#if setup && !setup.ready}
  <div class="card setup">
    <b>Getting Spool ready</b>
    <ol>
      {#each setup.items as i (i.id)}
        <li class:done={i.done}>
          <span class="tick" aria-hidden="true">{i.done ? '✓' : ''}</span>
          <span class="grow">{#if i.done}{i.label}{:else}<a class="link" href={i.href}>{i.label}</a>{#if i.optional}<span class="faint"> · optional</span>{/if}{#if i.detail}<span class="faint small"> · {i.detail}</span>{/if}{/if}</span>
        </li>
      {/each}
    </ol>
  </div>
{/if}

<style>
  .setup {
    padding: 14px 16px;
    margin-bottom: 18px;
    display: grid;
    gap: 8px;
  }
  ol {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 6px;
  }
  li {
    display: flex;
    gap: 10px;
    align-items: baseline;
  }
  li.done {
    color: var(--muted);
  }
  .tick {
    width: 18px;
    height: 18px;
    flex: none;
    border: 1px solid var(--line);
    border-radius: 50%;
    display: inline-grid;
    place-items: center;
    font-size: 11px;
    color: var(--ok, var(--accent));
  }
  .grow {
    flex: 1;
    min-width: 0;
  }
</style>
