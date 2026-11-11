// Which link carries the samples (pure; apps/desktop/ui/tests/link-pill.test.ts): the pill after the free space of the Target row and the suffix of the
// progress line. The desktop backend sets the state (samples-backend.ts); nothing here talks to a device.
import { loadFailureShort, loadFailureText, type LoadFailure } from '@digi/dk-session';
import type { RowTag } from './transfer-core.ts';

/** `bulk`: the USB bulk link of usb-fast; `sysex`: no bulk link for this target; `fallback`: bulk failed during the last run (`reason`), SysEx took over. */
export interface LinkState {
  state: 'bulk' | 'sysex' | 'fallback';
  reason?: string;
  /** `sysex` only: usb-fast is selected but did not load at the last boot (the device's LOADS record); `loadFailureText` says why. */
  failure?: LoadFailure;
  /** `bulk` only: the device is taken over (it was in USB AUDIO/MIDI and runs in USB MIDI until the linger ends), so its USB audio is paused. */
  takenOver?: boolean;
}

/** The pill of the Target row; undefined while the backend has not been opened for the target. */
export function linkTag(link: LinkState | undefined): RowTag | undefined {
  if (!link) return undefined;
  switch (link.state) {
    case 'bulk':
      if (link.takenOver)
        return {
          text: 'FAST USB (USB audio paused)',
          tone: 'ok',
          title:
            'The device is in USB AUDIO/MIDI. For this transfer the workbench switched it to USB MIDI and samples go over the bulk interface of the usb-fast module. USB audio is paused and returns about 10 s after the last transfer; the stored USB CONFIG is not changed.',
        };
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
      return {
        text: 'SYSEX',
        tone: 'dim',
        title: 'Samples go over MIDI SysEx. For faster transfers, install the usb-fast module on the device.',
      };
  }
}

/** The end of the progress line of a running upload: ` · via Fast USB` or ` · via SysEx` ('' while the link is unknown). */
export function viaSuffix(link: LinkState | undefined): string {
  if (!link) return '';
  return link.state === 'bulk' ? ' · via Fast USB' : ' · via SysEx';
}
