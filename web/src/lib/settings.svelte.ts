// The settings, as every page of Settings shares them. Each page changes only what it shows,
// by sending those settings alone, so two pages saving at once keep each other's changes.
// Secrets come back blank, with whether each is set (`conf.g.secrets`); one sent blank is
// kept, and one sent as null is cleared.
import { api } from './api';
import { act, loadStatus } from './state.svelte';

export const conf = $state<{ g: any | null }>({ g: null });

export async function loadSettings() {
  conf.g = await api.get('/settings').catch(() => conf.g);
  return conf.g;
}

/** Save some settings; true when they were taken. What is on the page is kept when not. */
export async function saveSettings(change: Record<string, unknown>, said = 'Saved'): Promise<boolean> {
  if ((await act(() => api.patch('/settings', change), said)) === undefined) return false;
  await loadSettings();
  loadStatus();
  return true;
}

/** Of `g`, the settings named, as numbers where they are numbers in the form. */
export function pick(g: any, keys: string[], numbers: string[] = []): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const k of keys) out[k] = numbers.includes(k) ? Number(g[k]) || 0 : g[k];
  return out;
}
