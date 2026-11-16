// What the card-folder strip (CardPanel.tsx) asks of the emulator and the system: the `card.*` methods of the controller (through the host's `rpc`), the folder dialog
// and the file manager. The panel (CardPanel.tsx) keeps the state and the log; this module only asks.
import { pickStart, type CardFolder, type OverlayReset } from './card-core';
import type { Host } from '@digi/workbench-core/host/host.ts';

export type CardHost = Pick<Host, 'rpc' | 'fs' | 'shell'>;

export function cardApi(host: CardHost) {
  const { rpc } = host;
  return {
    folder: () => rpc.call<CardFolder>('card.folder'),
    sync: () => rpc.call<CardFolder>('card.sync'),
    /** The folder dialog, then the choice; undefined when the user cancels. */
    async choose(card: CardFolder | undefined, title: string): Promise<CardFolder | undefined> {
      const dir = await host.fs.pickFolder({ start: card ? pickStart(card) : null, title });
      return dir ? rpc.call<CardFolder>('card.folder_set', { dir }) : undefined;
    },
    useDefault: () => rpc.call<CardFolder>('card.folder_set', {}),
    resetOverlay: () => rpc.call<OverlayReset>('card.reset_overlay'),
    openFolder: () => host.shell.openCardFolder(),
  };
}
