// Which link carries the samples (pure; apps/desktop/ui/tests/link-pill.test.ts): the pill after the free space of the Target row and the suffix of the
// progress line. The desktop backend sets the state (samples-backend.ts); nothing here talks to a device.
import type { RowTag } from './transfer-core.ts';

/** `bulk`: the USB bulk link of usb-fast; `sysex`: no bulk link for this target; `fallback`: bulk failed during the last run (`reason`), SysEx took over. */
export interface LinkState {
  state: 'bulk' | 'sysex' | 'fallback';
  reason?: string;
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
      return {
        text: 'SYSEX',
        tone: 'dim',
        title: 'Samples go over MIDI SysEx. Install the usb-fast module on the device for faster transfers.',
      };
  }
}

/** The end of the progress line of a running upload: ` · via Fast USB` or ` · via SysEx` ('' while the link is unknown). */
export function viaSuffix(link: LinkState | undefined): string {
  if (!link) return '';
  return link.state === 'bulk' ? ' · via Fast USB' : ' · via SysEx';
}
