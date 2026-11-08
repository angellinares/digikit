// The pure part of the loading state in Console.tsx: what the status bar and the screen area say while a firmware starts.
// Host tests: apps/desktop/ui/tests/boot.test.ts.

export type BootDevice = 'dt2' | 'dn2';
/** A firmware that is starting: its file name, and the device when the name tells it. */
export type Booting = { name: string; device?: BootDevice };

export const DEVICE_NAMES: Record<BootDevice, string> = { dt2: 'Digitakt II', dn2: 'Digitone II' };

/** The device a firmware file is for, from its name (Digitakt_II_OS1.17.syx), or undefined when the name does not say. */
export function deviceFromName(name: string): BootDevice | undefined {
  if (/digitakt|dt2/i.test(name)) return 'dt2';
  if (/digitone|dn2/i.test(name)) return 'dn2';
  return undefined;
}

export function booting(name: string): Booting {
  return { name, device: deviceFromName(name) };
}

/** The status bar text: the device name once known, else the file name, else a bare `Booting…`. */
export function bootingStatus(b: Booting): string {
  const what = b.device ? DEVICE_NAMES[b.device] : b.name;
  return what ? `Booting ${what}…` : 'Booting…';
}

/** The screen area text. */
export function bootingScreen(b: Booting): string {
  return `${bootingStatus(b)} the first boot of a firmware can take a minute`;
}

/** The status bar text for a failed load. */
export function loadFailure(name: string, error: string): string {
  return `Loading ${name || 'the firmware'} failed: ${error}`;
}
