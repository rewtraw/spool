<script lang="ts">
  import type { Job } from '../lib/api';
  import { bytes, duration, pct, speed, STATE_LABEL } from '../lib/format';

  let { job, state, error, size = 0 }: { job?: Job | null; state: string; error?: string | null; size?: number } = $props();

  // Say what is happening in one line a person can read.
  const line = $derived.by(() => {
    if (state === 'import_blocked') return error ?? 'Waiting for a decision';
    if (state === 'importing') return error ?? 'Moving into the library';
    if (state === 'failed') return error ?? 'Failed';
    // No live job yet for something marked as downloading means it is waiting its turn.
    if (!job) return [error ?? (state === 'downloading' ? 'Waiting in the queue' : (STATE_LABEL[state] ?? state)), size ? bytes(size) : ''].filter(Boolean).join(' · ');
    const parts: string[] = [];
    if (job.state === 'downloading') {
      parts.push(`${bytes(job.done_bytes)} of ${bytes(job.total_bytes)}`);
      if (job.speed) parts.push(speed(job.speed));
      if (job.eta_secs != null) parts.push(`${duration(job.eta_secs)} left`);
    } else {
      parts.push(job.state === 'queued' ? 'Waiting in the queue' : job.message || STATE_LABEL[job.state] || job.state);
      // Say how big it is even when nothing is moving, and how far a paused one got.
      const total = job.total_bytes || size;
      if (total) parts.push(job.done_bytes > 0 && job.done_bytes < job.total_bytes ? `${bytes(job.done_bytes)} of ${bytes(total)}` : bytes(total));
    }
    const missing = job.missing_articles + job.damaged_articles;
    if (missing > 0) parts.push(`${missing} ${missing === 1 ? 'article' : 'articles'} missing`);
    return parts.join(' · ');
  });
  const showBar = $derived(job && ['downloading', 'queued', 'paused'].includes(job.state) && state === 'downloading');
</script>

<div class="jobline">
  <div class="text" class:bad={state === 'failed' || state === 'import_blocked'}>{line}</div>
  {#if showBar && job}
    <div class="progress" role="progressbar" aria-valuenow={Math.round(pct(job.done_bytes, job.total_bytes))} aria-valuemin="0" aria-valuemax="100">
      <i style="width:{pct(job.done_bytes, job.total_bytes)}%"></i>
    </div>
  {/if}
</div>

<style>
  .jobline {
    display: grid;
    gap: 6px;
  }
  .text {
    font-size: 13.5px;
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }
  .text.bad {
    color: var(--bad);
  }
</style>
