import fs from 'node:fs';
import path from 'node:path';
import { repoRoot, type Device } from '@digi/test-support';

export const DEVICES = ['dt2', 'dn2'] as const;
export const FIRMWARE = { dt2: '1.17', dn2: '1.12' } as const;

export function liveOptions(device: Device, extra: { patchSet?: string; mirror?: boolean } = {}) {
  const binary = process.env[device === 'dt2' ? 'DIGI_EMUD_DT2' : 'DIGI_EMUD_DN2'];
  if (!binary) throw new Error(`DIGI_EMUD_${device.toUpperCase()} must name a preflighted daemon`);
  const card = device === 'dt2' ? path.join(repoRoot(), 'out/runs/patchkit/samples') : undefined;
  if (card && !fs.existsSync(card)) {
    throw new Error(
      `DT2 sample card missing: generate ${card} (tools.emulator.card_check.make_samples, run by pk_harness)`,
    );
  }
  return { device, firmware: FIRMWARE[device], binary, card, ...extra };
}

/** Full-tier scenarios run only under `DIGIKIT_TIER=full` (`verify --full`), as the Python tiers do. */
export const FULL_TIER = process.env.DIGIKIT_TIER === 'full';
