// Saving a video or picture from Spool to the device: on a phone through the share sheet,
// where "Save Video" puts it straight into Photos, and elsewhere as a download.

import { toast } from './state.svelte';

/** Whether this browser can hand files to a share sheet. */
export const canShareFiles = () => typeof navigator.canShare === 'function';

/** Hand a file over: to the share sheet where there is one, and as a download where there is
 * not. Gives false when the share sheet would not open because fetching the file took long
 * enough that the tap no longer counts; the next tap should hand the same file over. */
export async function deliver(file: File): Promise<boolean> {
  if (canShareFiles() && navigator.canShare({ files: [file] })) {
    try {
      await navigator.share({ files: [file] });
    } catch (err: any) {
      if (err?.name === 'NotAllowedError') return false;
      if (err?.name !== 'AbortError') throw err;
    }
    return true;
  }
  const a = document.createElement('a');
  a.href = URL.createObjectURL(file);
  a.download = file.name;
  a.click();
  setTimeout(() => URL.revokeObjectURL(a.href), 60000);
  return true;
}

/** Fetch a file from Spool as a File by this name. */
export async function fetchFile(url: string, name: string, type: string): Promise<File> {
  const res = await fetch(url);
  if (!res.ok) throw new Error((await res.json().catch(() => null))?.error ?? `It could not be fetched (${res.status})`);
  return new File([await res.blob()], name, { type });
}

/** A name made safe to save a file by. */
export const safeName = (name: string, fallback: string) => name.replace(/[/\\:"'\n\r]+/g, ' ').replace(/\s+/g, ' ').trim() || fallback;

/** Put text on the clipboard. Spool is reached over plain http, where browsers keep the
 * clipboard's own interface away from pages, so the old way is the one that works. */
export function copy(text: string) {
  const area = document.createElement('textarea');
  area.value = text;
  area.setAttribute('readonly', '');
  area.style.position = 'fixed';
  area.style.opacity = '0';
  document.body.appendChild(area);
  area.select();
  area.setSelectionRange(0, text.length);
  let done = false;
  try {
    done = document.execCommand('copy');
  } catch {
    done = false;
  }
  area.remove();
  return done;
}

/** Hand a link over: to the share sheet where there is one, onto the clipboard where not. */
export async function shareLink(url: string, title: string) {
  if (typeof navigator.share === 'function') {
    try {
      await navigator.share({ url, title });
      return;
    } catch (err: any) {
      if (err?.name === 'AbortError') return;
    }
  }
  if (copy(url)) toast('Link copied. Anyone with it can watch, until it is no longer shared.');
  else toast(`The link could not be copied: ${url}`, 'error');
}
