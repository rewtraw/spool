<script lang="ts">
  import type { Snippet } from 'svelte';
  import type { Watched } from '../lib/api';
  import Poster from './Poster.svelte';

  // One poster in a grid, with a name and a line under it: a title in the library, a
  // collection (drawn as a stack), or a member of a collection.
  let {
    href,
    src,
    title,
    kind = 'movie',
    sub = '',
    chip = '',
    tone = '',
    dim = false,
    picked = false,
    stack = false,
    watched,
    onclick,
    children,
  }: {
    href: string;
    src?: string | null;
    title: string;
    kind?: string;
    /** The line under the name. */
    sub?: string;
    /** A label pinned to the poster's corner, and its colour. */
    chip?: string;
    tone?: string;
    dim?: boolean;
    picked?: boolean;
    stack?: boolean;
    /** Where whoever is signed in stands with it: a tick when watched, a bar when part-way. */
    watched?: Watched;
    onclick?: (e: MouseEvent) => void;
    /** Anything else to lay over the poster. */
    children?: Snippet;
  } = $props();
</script>

<a class="tile" {href} class:dim class:picked {onclick}>
  <div class="art" class:stack>
    <Poster {src} {title} {kind} />
    {@render children?.()}
    {#if chip}<span class="chip {tone} pin">{chip}</span>{/if}
    {#if watched?.done}<span class="seen" title="Watched" aria-label="Watched">✓</span>
    {:else if watched?.part}<span class="part" title="{Math.round(watched.part * 100)}% watched"><i style="width:{Math.max(4, watched.part * 100)}%"></i></span>{/if}
  </div>
  <div class="name truncate">{title}</div>
  <div class="faint small">{sub}</div>
</a>

<style>
  .tile {
    display: grid;
    gap: 4px;
    min-width: 0;
  }
  .art {
    position: relative;
    transition: transform 0.15s ease;
  }
  .tile:focus-visible .art {
    transform: translateY(-3px);
  }
  @media (hover: hover) {
    .tile:hover .art {
      transform: translateY(-3px);
    }
  }
  .tile.dim .art {
    opacity: 0.55;
  }
  .tile.picked .art {
    outline: 3px solid var(--accent);
    outline-offset: 2px;
    border-radius: 10px;
  }
  /* A collection: a poster with the edges of others behind it. */
  .art.stack {
    margin: 0 6px 0 0;
  }
  .art.stack::before,
  .art.stack::after {
    content: '';
    position: absolute;
    inset: 4px -4px 4px 4px;
    border-radius: 8px;
    border: 1px solid var(--line);
    background: var(--raised);
    z-index: -1;
  }
  .art.stack::after {
    inset: 9px -8px 9px 8px;
    z-index: -2;
    opacity: 0.6;
  }
  .pin {
    position: absolute;
    left: 7px;
    bottom: 7px;
    backdrop-filter: blur(8px);
    background: color-mix(in srgb, var(--bg) 78%, transparent);
  }
  /* Watched: a tick in the corner. Part-way: how far, along the foot of the poster. */
  .seen {
    position: absolute;
    top: 7px;
    right: 7px;
    width: 22px;
    height: 22px;
    display: grid;
    place-items: center;
    border-radius: 50%;
    font-size: 12px;
    font-weight: 700;
    color: #fff;
    background: var(--ok);
    box-shadow: 0 1px 4px rgba(0, 0, 0, 0.35);
  }
  .part {
    position: absolute;
    left: 6px;
    right: 6px;
    bottom: 5px;
    height: 4px;
    border-radius: 2px;
    background: rgba(0, 0, 0, 0.5);
    overflow: hidden;
  }
  .part i {
    display: block;
    height: 100%;
    background: var(--accent);
  }
  .chip.pin + .part {
    bottom: 34px;
  }
  .name {
    font-weight: 600;
    font-size: 14px;
    margin-top: 4px;
  }
  .small {
    font-size: 12.5px;
  }
</style>
