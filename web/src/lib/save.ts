// Saving a video or picture from Spool to the device: on a phone through the share sheet,
// where "Save Video" puts it straight into Photos, and elsewhere as a download.

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
