// Which link carries the samples (pure; apps/desktop/ui/tests/link-pill.test.ts): the pill after the free space of the Target row and the suffix of the
// progress line. The desktop backend sets the state (samples-backend.ts); nothing here talks to a device.
import { loadFailureShort, loadFailureText, type LoadFailure } from '@digi/dk-session';
import type { RowTag } from './transfer-core.ts';

/** `bulk`: the USB bulk link of usb-fast; `sysex`: no bulk link for this target; `fallback`: bulk failed during the last run (`reason`), SysEx took over. */
export interface LinkState {
  state: 'bulk' | 'sysex' | 'fallback';
  /** `fallback`: why the bulk link stopped. `sysex`: why the bulk link was not chosen (the last note of the choice), when usb-fast is loaded. */
  reason?: string;
  /** `sysex` only: usb-fast is selected but did not load at the last boot (the device's LOADS record); `loadFailureText` says why. */
  failure?: LoadFailure;
  /** `sysex` only: usb-fast is loaded but the device is in USB AUDIO/MIDI mode and offers no bulk interface (firmware without core/usb-bulk-audio). */
  wrongMode?: boolean;
}

/** The pill of the Target row; undefined while the backend has not been opened for the target. */
export function linkTag(link: LinkState | undefined): RowTag | undefined {
  if (!link) return undefined;
  switch (link.state) {
    case 'bulk':
      return {
        text: 'FAST USB',
        tone: 'ok',
        title: 'Samples go over the USB bulk interface of the usb-fast module. Everything else uses SysEx.',
      };
    case 'fallback':
      return {
        text: 'SYSEX: FALLBACK',
        tone: 'warn',
        title: `The fast USB link stopped during this run (${link.reason || 'no reason logged'}). The rest of the run went over SysEx. The next run tries Fast USB again.`,
      };
    default:
      if (link.failure)
        return { text: `SYSEX: ${loadFailureShort(link.failure)}`, tone: 'warn', title: loadFailureText(link.failure) };
      if (link.wrongMode) return { text: 'SYSEX', tone: 'warn', title: linkHint(link) ?? '' };
      if (link.reason) return { text: 'SYSEX', tone: 'warn', title: linkHint(link) ?? '' };
      return {
        text: 'SYSEX',
        tone: 'dim',
        title: 'Samples go over MIDI SysEx. For faster transfers, install the usb-fast module on the device.',
      };
  }
}

/** One short visible sentence under the pill that says why samples do not use Fast USB; null for a bulk link. */
export function linkHint(link: LinkState | undefined): string | null {
  if (!link || link.state === 'bulk') return null;
  if (link.state === 'fallback')
    return `Fast USB stopped during this run (${link.reason || 'no reason logged'}); the rest went over SysEx.`;
  if (link.failure) return loadFailureText(link.failure);
  if (link.wrongMode)
    return 'This firmware has no Fast USB interface in USB AUDIO/MIDI. Flash firmware from the Firmware page, or set USB CONFIG to USB MIDI on the device.';
  if (link.reason) return `Fast USB not used: ${link.reason}`;
  return 'Install the usb-fast module for Fast USB.';
}

/** The end of the progress line of a running upload: ` · via Fast USB` or ` · via SysEx` ('' while the link is unknown). */
export function viaSuffix(link: LinkState | undefined): string {
  if (!link) return '';
  return link.state === 'bulk' ? ' · via Fast USB' : ' · via SysEx';
}
