// TargetOps is the one facade the workbench uses for a target (the emulator or a device). `store` works on both targets over DkSession, so they
// share one code path. `run` is a stub to fill when pause, reset and restart move here (the emulator through EmulatorRuntime; the device only
// restart, through the protocol).
import { fmtVersion, type DkEntry, type DkInfoFull, type DkModule, type ModuleState } from '@digi/dk-session';
import type { Target } from '../transfer/target';
import type { DkmHeader, StoreUsage } from './store';

export interface StoreFacts {
  device: string;
  /** "major.minor.patch" of the digikit runtime. */
  runtime: string;
  protocol: number;
  caps: number | null;
  cardReadOnly: boolean;
  safeMode: boolean;
  imageId: string | null;
  svcAbi: number | null;
}

/** The facts of an INFO reply. */
export const factsOf = (info: DkInfoFull): StoreFacts => ({
  device: info.port,
  runtime: fmtVersion(info.runtime),
  protocol: info.protocol,
  caps: info.caps,
  cardReadOnly: info.card_read_only,
  safeMode: info.safe_mode,
  imageId: info.image_id,
  svcAbi: info.svc_abi,
});

export interface StoreSnapshot {
  facts: StoreFacts;
  entries: DkEntry[];
  usage: StoreUsage;
  modules: DkModule[];
  states: ModuleState[];
}

/** Read-only access to the card store of a target. */
export interface StoreOps {
  snapshot(): Promise<StoreSnapshot>;
  header(path: string): Promise<DkmHeader | null>;
  /** The file size and up to READ_MAX bytes from `offset`. */
  read(path: string, offset: number, length: number): Promise<{ size: number; bytes: Uint8Array }>;
  readAll(path: string, onProgress?: (done: number, size: number) => void): Promise<Uint8Array>;
  /** The hash slot (hex) the device computes from a fresh read of the card. */
  verify(path: string): Promise<string>;
}

export type RunOp = 'pause' | 'resume' | 'reset' | 'restart';

/** Run control of a target. A method is absent when the target cannot do it; `why` says why (null: the operation is there). */
export interface RunOps {
  pause?(): Promise<void>;
  resume?(): Promise<void>;
  reset?(): Promise<void>;
  restart?(): Promise<void>;
  why(op: RunOp): string | null;
}

export interface TargetOps {
  readonly target: Target;
  readonly store: StoreOps;
  readonly run: RunOps;
}

export function unwiredRunOps(): RunOps {
  return { why: () => 'Run control does not go through the target layer yet: use the Emulator page.' };
}
