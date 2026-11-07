// The pure logic of the Card panel (no DOM, no Tauri): the folder status of `card.folder` and its texts.
// Host tests: apps/desktop/ui/tests/card.test.ts.

export interface CardFile {
  /** Relative to `digikit/` of the folder. */
  path: string;
  bytes: number;
  hash: string;
}

/** The answer of the controller's `card.folder` / `card.sync`. */
export interface CardFolder {
  folder: string | null;
  /** The module store is mirrored to the folder while the emulator runs. */
  continuous: boolean;
  files: CardFile[];
  /** ms since the epoch of the last pass, or null. */
  last_sync: number | null;
  /** The card has writes the folder does not hold yet. */
  behind?: boolean;
  error: string | null;
  /** The loaded device (`dt2`, `dn2`): the folder belongs to it. */
  device?: string;
  /** `plus_drive/<device>/`. */
  default_folder?: string | null;
  /** The user chose the folder (the Card panel's "Choose folder..."), else it is the default. */
  custom?: boolean;
  /** A saved choice whose folder is gone: the default is used instead. */
  chosen_missing?: string | null;
  /** The folder the next load uses. */
  next_folder?: string | null;
  /** `next_folder` is not `folder` yet: the device's card is built from the folder, so it takes the new one at the next load. */
  reload_needed?: boolean;
}

/** "never", "just now", "12 s ago", "3 min ago", "2 h ago". */
export function agoText(lastSync: number | null, now: number): string {
  if (lastSync === null) return 'never';
  const s = Math.max(0, Math.round((now - lastSync) / 1000));
  if (s < 3) return 'just now';
  if (s < 60) return `${s} s ago`;
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  return `${Math.floor(s / 3600)} h ago`;
}

/** The status line under the file list. */
export function statusText(c: CardFolder, now: number): string {
  if (!c.continuous) return 'The card is not mirrored to a folder.';
  if (c.error) return `Sync failed: ${c.error}`;
  const when = `Last sync ${agoText(c.last_sync, now)}`;
  return c.behind ? `${when}; the card has changes not synced yet` : when;
}

/** The folder's shortened path for the header: the last two parts, the whole path stays the tooltip. */
export function shortFolder(folder: string | null): string {
  if (!folder) return '';
  const parts = folder.split('/').filter(Boolean);
  return parts.length > 2 ? `…/${parts.slice(-2).join('/')}` : folder;
}

/** The files as shown: the module store files by path. */
export function fileRows(c: CardFolder): CardFile[] {
  return [...c.files].sort((a, b) => a.path.localeCompare(b.path));
}

/** A top folder of the store and its files. */
export interface CardGroup {
  /** `modules`, `samples`, ... ('' for a file at the top). */
  folder: string;
  files: CardFile[];
  bytes: number;
}

/** The files grouped by top folder, folders by name (the mirror lists the whole store flat). */
export function fileGroups(c: CardFolder): CardGroup[] {
  const by = new Map<string, CardFile[]>();
  for (const f of fileRows(c)) {
    const slash = f.path.indexOf('/');
    const folder = slash < 0 ? '' : f.path.slice(0, slash);
    by.set(folder, [...(by.get(folder) ?? []), f]);
  }
  return [...by]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([folder, files]) => ({ folder, files, bytes: files.reduce((n, f) => n + f.bytes, 0) }));
}

/** The name of a file inside its group: the path without the top folder. */
export const nameInGroup = (path: string, folder: string): string => (folder ? path.slice(folder.length + 1) : path);

/** What waits for a reload, or what is wrong with the saved choice; '' when there is nothing to say. */
export function folderNotice(c: CardFolder): string {
  if (c.reload_needed && c.next_folder) {
    return `The card is built from its folder when it loads. A reload makes it use ${shortFolder(c.next_folder)}; until then changes go to ${shortFolder(c.folder)}.`;
  }
  if (c.chosen_missing)
    return `The folder you chose, ${shortFolder(c.chosen_missing)}, is missing. The default folder is used.`;
  return '';
}

/** "Use default" is offered while a folder of the user's is saved for the device (even a missing one). */
export const canUseDefault = (c: CardFolder): boolean => !!c.custom || !!c.chosen_missing;

/** The folder the "Choose folder..." dialog starts in: the one the card uses next. */
export const pickStart = (c: CardFolder): string | null => c.next_folder ?? c.folder;
