<script lang="ts">
  let { src, title, kind = 'movie' }: { src?: string | null; title: string; kind?: string } = $props();
  let failed = $state(false);
  // Images already in the browser's cache are shown at once; only a first load fades in, so
  // filtering or re-sorting a grid never makes its posters blink.
  let fresh = $state(false);

  function watch(img: HTMLImageElement) {
    if (!img.complete) fresh = true;
  }
</script>

<div class="poster" class:fallback={!src || failed}>
  {#if src && !failed}
    <img {src} alt="" loading="lazy" width="400" height="600" class:fresh use:watch onload={(e) => e.currentTarget.classList.add('ready')} onerror={() => (failed = true)} />
  {:else}
    <span>{title}</span>
    <small>{kind === 'movie' ? 'Film' : 'Series'}</small>
  {/if}
</div>

<style>
  .poster {
    aspect-ratio: 2 / 3;
    border-radius: 8px;
    overflow: hidden;
    background: var(--raised);
    border: 1px solid var(--line);
    width: 100%;
  }
  img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
  }
  img.fresh {
    opacity: 0;
    transition: opacity 0.18s ease;
  }
  img.fresh:global(.ready) {
    opacity: 1;
  }
  .fallback {
    display: flex;
    flex-direction: column;
    justify-content: space-between;
    padding: 12px;
    background: linear-gradient(160deg, var(--raised), var(--surface));
  }
  span {
    font: 600 15px/1.25 var(--serif);
    overflow-wrap: anywhere;
    display: -webkit-box;
    -webkit-line-clamp: 5;
    line-clamp: 5;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  small {
    color: var(--faint);
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
</style>
