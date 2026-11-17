// The names of the runtime log over the Host. One path for both origins: the load table is read through PEEK of the debug module on the session
// the follower already uses (the emulator's USB capture or the device's MIDI port), and dkprof.wasm names the addresses. Only the source of the
// ship map differs:
//   emulator  the controller call `runtime.maps` (the live image; it follows `dev.reload`)
//   device    the ship map `firmware_prepare` kept for the image id the device reports in INFO (`library.readMaps`)
// Maps of the built modules come from `library.readMaps` for both.
import type { Host } from '@digi/workbench-core/host/host.ts';
import type { DkProf } from '@digi/dk-session/dkprof';
import { createLogNames, type LogNames, type NameSources, type RuntimeMaps } from '@digi/dk-session/log-names';
import { deviceOfPort } from './remote-core';

const sources = (host: Pick<Host, 'library'>, prof: () => Promise<DkProf>): Omit<NameSources, 'ship'> => ({
  prof,
  moduleMaps: async (device) => (await host.library.readMaps(device)).modules.map((m) => m.json),
});

export function createEmulatorNames(host: Pick<Host, 'rpc' | 'library'>, prof: () => Promise<DkProf>): LogNames {
  return createLogNames({
    ...sources(host, prof),
    ship: async () => {
      const maps = await host.rpc.call<RuntimeMaps>('runtime.maps');
      return { ship: maps.ship, symbols: maps.symbols, matches: maps.matches };
    },
  });
}

export function createDeviceNames(host: Pick<Host, 'library'>, prof: () => Promise<DkProf>): LogNames {
  return createLogNames({
    ...sources(host, prof),
    ship: async (session) => {
      const info = await session.info();
      const device = deviceOfPort(info.port ?? '');
      if (!device || !info.image_id)
        return { unavailable: 'the device does not report its image id (flash it from the Firmware page)' };
      const record = (await host.library.readMaps(device)).ships.find((s) => s.image_id === info.image_id);
      return record
        ? { ship: record.ship, symbols: record.symbols }
        : { unavailable: `no map for the image ${info.image_id} (flash it from the Firmware page of this workbench)` };
    },
  });
}
